//! User-owned dictionaries and bounded, asynchronous learning persistence.
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::engine::{promote_order, LearningEvent, PromotionEvent, RankingOrder};

pub const SCHEMA_VERSION: u32 = 3;
const MAX_AUTO_PHRASES: usize = 4096;
const AUTO_PHRASE_WEIGHT: u32 = 16384;
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RECORDS: usize = 100_000;
pub const MAX_LEARNING_COUNT: u32 = 8192;
const MAX_BATCH: usize = 1024;
const QUEUE_CAPACITY: usize = 32;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Messages deliberately never contain user entries, paths, or input.
#[derive(Debug, Error)]
pub enum DictionaryError {
    #[error("user dictionary I/O failed")]
    Io(#[from] io::Error),
    #[error("invalid user dictionary data")]
    InvalidData,
    #[error("unsupported user dictionary schema")]
    UnsupportedVersion,
    #[error("user dictionary resource limit exceeded")]
    LimitExceeded,
    #[error("unsafe user dictionary path")]
    UnsafePath,
    #[error("user dictionary lock timed out")]
    LockTimeout,
    #[error("user dictionary domain does not exist")]
    UnknownDomain,
    #[error("user dictionary generation exhausted")]
    GenerationExhausted,
    #[error("user dictionary learning queue is full")]
    QueueFull,
    #[error("user dictionary learning worker failed")]
    WorkerFailed,
    #[error("user dictionary learning worker has stopped")]
    WorkerStopped,
    #[error("user dictionary is unsupported on this platform")]
    UnsupportedPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DictionaryEntry {
    pub pinyin: String,
    pub text: String,
    pub weight: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainDictionary {
    pub name: String,
    pub enabled: bool,
    pub entries: Vec<DictionaryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearnedEntry {
    pub pinyin: String,
    pub text: String,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoPhrase {
    pub pinyin: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDictionary {
    pub schema_version: u32,
    pub generation: u64,
    pub learning_enabled: bool,
    #[serde(default = "default_adaptive_ranking_enabled")]
    pub adaptive_ranking_enabled: bool,
    #[serde(default)]
    pub rankings: Vec<RankingOrder>,
    pub entries: Vec<DictionaryEntry>,
    pub domains: Vec<DomainDictionary>,
    pub learned: Vec<LearnedEntry>,
    #[serde(default)]
    pub auto_phrases: Vec<AutoPhrase>,
}

impl Default for UserDictionary {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            generation: 0,
            learning_enabled: false,
            adaptive_ranking_enabled: true,
            rankings: Vec::new(),
            entries: Vec::new(),
            domains: Vec::new(),
            learned: Vec::new(),
            auto_phrases: Vec::new(),
        }
    }
}

impl UserDictionary {
    /// A canonical TSV snapshot; learned counts are not static frequencies.
    pub fn lexicon_tsv(&self) -> Result<String, DictionaryError> {
        self.snapshot_tsv(true)
    }

    pub fn base_lexicon_tsv(&self) -> Result<String, DictionaryError> {
        self.snapshot_tsv(false)
    }

    pub fn auto_phrase_tsv(&self) -> Result<String, DictionaryError> {
        self.validate()?;
        auto_phrases_tsv(&self.auto_phrases)
    }

    fn snapshot_tsv(&self, include_auto: bool) -> Result<String, DictionaryError> {
        self.validate()?;
        let entries = self.entries.iter().chain(
            self.domains
                .iter()
                .filter(|d| d.enabled)
                .flat_map(|d| &d.entries),
        );
        // Resolve duplicates deterministically without adding their weights.
        let mut merged: BTreeMap<(&str, &str), u32> = BTreeMap::new();
        for entry in entries {
            merged
                .entry((&entry.pinyin, &entry.text))
                .and_modify(|w| *w = (*w).max(entry.weight))
                .or_insert(entry.weight);
        }
        if include_auto {
            for phrase in &self.auto_phrases {
                merged
                    .entry((&phrase.pinyin, &phrase.text))
                    .and_modify(|w| *w = (*w).max(AUTO_PHRASE_WEIGHT))
                    .or_insert(AUTO_PHRASE_WEIGHT);
            }
        }
        let mut output = String::new();
        for ((pinyin, text), weight) in merged {
            output.push_str(&format!("{pinyin}\t{text}\t{weight}\n"));
        }
        if output.len() > MAX_FILE_BYTES {
            return Err(DictionaryError::LimitExceeded);
        }
        Ok(output)
    }

    fn validate(&self) -> Result<(), DictionaryError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(DictionaryError::UnsupportedVersion);
        }
        validate_rankings(&self.rankings)?;
        validate_auto_phrases(&self.auto_phrases)?;
        if self.domains.len() > MAX_RECORDS
            || self
                .entries
                .len()
                .saturating_add(self.learned.len())
                .saturating_add(self.auto_phrases.len())
                .saturating_add(self.domains.iter().map(|d| d.entries.len()).sum::<usize>())
                .saturating_add(
                    self.rankings
                        .iter()
                        .map(|r| r.candidates.len())
                        .sum::<usize>(),
                )
                > MAX_RECORDS
        {
            return Err(DictionaryError::LimitExceeded);
        }
        validate_entries(&self.entries)?;
        let mut names = HashSet::new();
        for domain in &self.domains {
            validate_domain(&domain.name)?;
            if !names.insert(&domain.name) {
                return Err(DictionaryError::InvalidData);
            }
            validate_entries(&domain.entries)?;
        }
        let mut learned = HashSet::new();
        for entry in &self.learned {
            validate_pair(&entry.pinyin, &entry.text)?;
            if entry.count == 0
                || entry.count > MAX_LEARNING_COUNT
                || !learned.insert((&entry.pinyin, &entry.text))
            {
                return Err(DictionaryError::InvalidData);
            }
        }
        Ok(())
    }
}

fn default_adaptive_ranking_enabled() -> bool {
    true
}

fn validate_auto_pair(pinyin: &str, text: &str) -> Result<(), DictionaryError> {
    validate_pair(pinyin, text)?;
    if !(2..=32).contains(&text.chars().count())
        || !text.chars().all(|c| matches!(c, '\u{4e00}'..='\u{9fff}'))
        || pinyin.bytes().filter(|b| *b != b'\'').count() > 128
        || !pinyin
            .split('\'')
            .all(|s| s.bytes().any(|b| b"aeiouv".contains(&b)))
    {
        return Err(DictionaryError::InvalidData);
    }
    Ok(())
}

fn validate_auto_phrases(phrases: &[AutoPhrase]) -> Result<(), DictionaryError> {
    if phrases.len() > MAX_AUTO_PHRASES {
        return Err(DictionaryError::LimitExceeded);
    }
    let mut seen = HashSet::new();
    for phrase in phrases {
        validate_auto_pair(&phrase.pinyin, &phrase.text)?;
        if !seen.insert((&phrase.pinyin, &phrase.text)) {
            return Err(DictionaryError::InvalidData);
        }
    }
    Ok(())
}

pub(crate) fn remember_auto_phrases(
    phrases: &mut Vec<AutoPhrase>,
    events: &[LearningEvent],
) -> Result<(), DictionaryError> {
    validate_auto_phrases(phrases)?;
    if events.len() > MAX_BATCH {
        return Err(DictionaryError::LimitExceeded);
    }
    for event in events {
        validate_auto_pair(&event.pinyin, &event.text)?;
    }
    for event in events {
        phrases.retain(|p| p.pinyin != event.pinyin || p.text != event.text);
        phrases.push(AutoPhrase {
            pinyin: event.pinyin.clone(),
            text: event.text.clone(),
        });
        if phrases.len() > MAX_AUTO_PHRASES {
            phrases.remove(0);
        }
    }
    Ok(())
}

pub(crate) fn auto_phrases_tsv(phrases: &[AutoPhrase]) -> Result<String, DictionaryError> {
    validate_auto_phrases(phrases)?;
    let mut output = String::new();
    for phrase in phrases {
        output.push_str(&format!(
            "{}\t{}\t{AUTO_PHRASE_WEIGHT}\n",
            phrase.pinyin, phrase.text
        ));
    }
    if output.len() > MAX_FILE_BYTES {
        return Err(DictionaryError::LimitExceeded);
    }
    Ok(output)
}

fn validate_commit(
    promotions: &[PromotionEvent],
    phrases: &[LearningEvent],
) -> Result<(), DictionaryError> {
    if promotions.len().saturating_add(phrases.len()) > MAX_BATCH {
        return Err(DictionaryError::LimitExceeded);
    }
    validate_promotions(promotions)?;
    for phrase in phrases {
        validate_auto_pair(&phrase.pinyin, &phrase.text)?;
    }
    // Include both halves in the queue's serialized payload budget.
    let pairs: Vec<_> = phrases.iter().map(|p| (&p.pinyin, &p.text)).collect();
    if serde_json::to_vec(&(promotions, pairs))
        .map_err(|_| DictionaryError::InvalidData)?
        .len()
        > MAX_FILE_BYTES
    {
        return Err(DictionaryError::LimitExceeded);
    }
    Ok(())
}

fn validate_order(order: &RankingOrder) -> Result<(), DictionaryError> {
    let input = &order.input;
    if input.is_empty()
        || input.len() > 128
        || input.starts_with('\'')
        || input.contains("''")
        || !input.bytes().all(|b| b.is_ascii_lowercase() || b == b'\'')
    {
        return Err(DictionaryError::InvalidData);
    }
    if order.candidates.len() > 4096 {
        return Err(DictionaryError::LimitExceeded);
    }
    let mut seen = HashSet::new();
    for candidate in &order.candidates {
        if candidate.pinyin.is_empty()
            || candidate.pinyin.len() > 256
            || !candidate
                .pinyin
                .split('\'')
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
            || candidate.text.is_empty()
            || candidate.text.chars().count() > 128
            || candidate.text.chars().any(char::is_control)
            || candidate.consumed != input.len()
            || !seen.insert((&candidate.pinyin, &candidate.text, candidate.consumed))
        {
            return Err(DictionaryError::InvalidData);
        }
    }
    Ok(())
}

fn validate_rankings(orders: &[RankingOrder]) -> Result<(), DictionaryError> {
    if orders.len() > 256 || orders.iter().map(|o| o.candidates.len()).sum::<usize>() > MAX_RECORDS
    {
        return Err(DictionaryError::LimitExceeded);
    }
    let mut inputs = HashSet::new();
    for order in orders {
        validate_order(order)?;
        if !inputs.insert(&order.input) {
            return Err(DictionaryError::InvalidData);
        }
    }
    Ok(())
}

// Bound borrowed data before cloning it into the bounded worker queue. The
// serialized-size check also accounts for JSON escaping and selected identities.
fn validate_promotions(events: &[PromotionEvent]) -> Result<(), DictionaryError> {
    if events.len() > MAX_BATCH {
        return Err(DictionaryError::LimitExceeded);
    }
    let mut ids = 0usize;
    let mut bytes = 0usize;
    for event in events {
        ids = ids.saturating_add(event.order.candidates.len());
        if ids > MAX_RECORDS {
            return Err(DictionaryError::LimitExceeded);
        }
        validate_order(&event.order)?;
        if !event.order.candidates.contains(&event.selected) {
            return Err(DictionaryError::InvalidData);
        }
        bytes = bytes.saturating_add(
            serde_json::to_vec(event)
                .map_err(|_| DictionaryError::InvalidData)?
                .len(),
        );
        if bytes > MAX_FILE_BYTES {
            return Err(DictionaryError::LimitExceeded);
        }
    }
    Ok(())
}

fn validate_domain(name: &str) -> Result<(), DictionaryError> {
    if name.is_empty()
        || name.len() > 256
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err(DictionaryError::InvalidData);
    }
    Ok(())
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff
        | 0x20000..=0x2a6df | 0x2a700..=0x2b73f | 0x2b740..=0x2b81f
        | 0x2b820..=0x2ceaf | 0x2ceb0..=0x2ebef | 0x2ebf0..=0x2ee5f
        | 0x2f800..=0x2fa1f | 0x30000..=0x3134f | 0x31350..=0x323af)
}

fn validate_pair(pinyin: &str, text: &str) -> Result<(), DictionaryError> {
    let mut syllables = 0;
    for syllable in pinyin.split('\'') {
        if syllable.is_empty()
            || syllable.len() > 6
            || !syllable.bytes().all(|b| b.is_ascii_lowercase())
        {
            return Err(DictionaryError::InvalidData);
        }
        syllables += 1;
    }
    if syllables > 32 || text.chars().count() != syllables || !text.chars().all(is_han) {
        return Err(DictionaryError::InvalidData);
    }
    Ok(())
}

fn normalize_pair(pinyin: &str, text: &str) -> Result<String, DictionaryError> {
    // Only ASCII spaces and apostrophes are separators, never raw input.
    let normalized = pinyin
        .trim_matches(' ')
        .split('\'')
        .map(|part| {
            part.split(' ')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("'")
        })
        .collect::<Vec<_>>()
        .join("'");
    validate_pair(&normalized, text)?;
    Ok(normalized)
}

fn validate_entries(entries: &[DictionaryEntry]) -> Result<(), DictionaryError> {
    let mut seen = HashSet::new();
    for entry in entries {
        validate_pair(&entry.pinyin, &entry.text)?;
        if entry.weight == 0 || !seen.insert((&entry.pinyin, &entry.text)) {
            return Err(DictionaryError::InvalidData);
        }
    }
    Ok(())
}

fn parse_tsv(bytes: &[u8]) -> Result<Vec<DictionaryEntry>, DictionaryError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DictionaryError::InvalidData)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut entries = BTreeMap::new();
    let mut records = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        records += 1;
        if records > MAX_RECORDS {
            return Err(DictionaryError::LimitExceeded);
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(DictionaryError::InvalidData);
        }
        let pinyin = normalize_pair(fields[0], fields[1])?;
        if fields[2].is_empty() || !fields[2].bytes().all(|b| b.is_ascii_digit()) {
            return Err(DictionaryError::InvalidData);
        }
        let weight: u32 = fields[2]
            .parse()
            .map_err(|_| DictionaryError::InvalidData)?;
        if weight == 0 {
            return Err(DictionaryError::InvalidData);
        }
        entries.insert((pinyin, fields[1].to_owned()), weight);
        if entries.len() > MAX_RECORDS {
            return Err(DictionaryError::LimitExceeded);
        }
    }
    Ok(entries
        .into_iter()
        .map(|((pinyin, text), weight)| DictionaryEntry {
            pinyin,
            text,
            weight,
        })
        .collect())
}

#[derive(Clone, Debug)]
pub struct DictionaryStore {
    path: PathBuf,
}

impl DictionaryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn for_current_user() -> Result<Self, DictionaryError> {
        let path = current_user_path()?;
        checked_path(&path)?;
        Ok(Self::new(path))
    }

    pub fn load(&self) -> Result<UserDictionary, DictionaryError> {
        let path = checked_path(&self.path)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(UserDictionary::default());
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        #[cfg(target_os = "windows")]
        let _lock = NamedMutex::acquire(&path, Instant::now() + Duration::from_secs(10))?;
        #[cfg(not(target_os = "windows"))]
        let _lock = StoreLock::acquire(&path)?;
        // The read handle is owned and closed inside load_path/read_bounded,
        // before this kernel-only Windows read guard is released. Updates
        // already hold StoreLock and call load_path directly, without nesting.
        load_path(&path)
    }

    fn update(
        &self,
        manage: bool,
        change: impl FnOnce(&mut UserDictionary) -> Result<bool, DictionaryError>,
    ) -> Result<(), DictionaryError> {
        let path = checked_path(&self.path)?;
        let parent = path.parent().ok_or(DictionaryError::UnsafePath)?;
        let _created_directories = create_parent(parent)?;
        let path = checked_path(&path)?;
        let _lock = StoreLock::acquire(&path)?;
        let mut dictionary = load_path(&path)?;
        if !change(&mut dictionary)? {
            return Ok(());
        }
        if manage {
            dictionary.generation = dictionary
                .generation
                .checked_add(1)
                .ok_or(DictionaryError::GenerationExhausted)?;
        }
        dictionary.validate()?;
        let bytes =
            serde_json::to_vec_pretty(&dictionary).map_err(|_| DictionaryError::InvalidData)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(DictionaryError::LimitExceeded);
        }
        atomic_write(&path, &bytes)
    }

    pub fn import_tsv(&self, path: &Path, domain: Option<&str>) -> Result<(), DictionaryError> {
        if let Some(name) = domain {
            validate_domain(name)?;
        }
        let incoming = parse_tsv(&read_bounded(&checked_path(path)?)?)?;
        self.update(true, |dictionary| {
            let entries = match domain {
                None => &mut dictionary.entries,
                Some(name) => {
                    let index = match dictionary.domains.iter().position(|d| d.name == name) {
                        Some(index) => index,
                        None => {
                            dictionary.domains.push(DomainDictionary {
                                name: name.to_owned(),
                                enabled: false,
                                entries: Vec::new(),
                            });
                            dictionary.domains.len() - 1
                        }
                    };
                    &mut dictionary.domains[index].entries
                }
            };
            let mut merged: BTreeMap<_, _> = entries
                .drain(..)
                .map(|e| ((e.pinyin, e.text), e.weight))
                .collect();
            for entry in incoming {
                merged.insert((entry.pinyin, entry.text), entry.weight);
            }
            *entries = merged
                .into_iter()
                .map(|((pinyin, text), weight)| DictionaryEntry {
                    pinyin,
                    text,
                    weight,
                })
                .collect();
            Ok(true)
        })
    }

    pub fn export_tsv(&self, path: &Path, domain: Option<&str>) -> Result<(), DictionaryError> {
        let dictionary = self.load()?;
        let entries = match domain {
            None => &dictionary.entries,
            Some(name) => {
                &dictionary
                    .domains
                    .iter()
                    .find(|d| d.name == name)
                    .ok_or(DictionaryError::UnknownDomain)?
                    .entries
            }
        };
        let mut output = String::new();
        for entry in entries {
            output.push_str(&format!(
                "{}\t{}\t{}\n",
                entry.pinyin, entry.text, entry.weight
            ));
        }
        if domain.is_none() {
            output = dictionary.lexicon_tsv()?;
        }
        if output.len() > MAX_FILE_BYTES {
            return Err(DictionaryError::LimitExceeded);
        }
        let path = checked_path(path)?;
        let mut file = secure_options().write(true).create_new(true).open(path)?;
        file.write_all(output.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    pub fn remove_entry(
        &self,
        pinyin: &str,
        text: &str,
        domain: Option<&str>,
    ) -> Result<(), DictionaryError> {
        let pinyin = normalize_pair(pinyin, text)?;
        self.update(true, |dictionary| {
            let entries = match domain {
                None => &mut dictionary.entries,
                Some(name) => {
                    &mut dictionary
                        .domains
                        .iter_mut()
                        .find(|d| d.name == name)
                        .ok_or(DictionaryError::UnknownDomain)?
                        .entries
                }
            };
            entries.retain(|e| e.pinyin != pinyin || e.text != text);
            Ok(true)
        })
    }

    pub fn set_domain_enabled(&self, name: &str, enabled: bool) -> Result<(), DictionaryError> {
        validate_domain(name)?;
        self.update(true, |dictionary| {
            dictionary
                .domains
                .iter_mut()
                .find(|d| d.name == name)
                .ok_or(DictionaryError::UnknownDomain)?
                .enabled = enabled;
            Ok(true)
        })
    }

    pub fn set_learning_enabled(&self, enabled: bool) -> Result<(), DictionaryError> {
        self.update(true, |dictionary| {
            dictionary.learning_enabled = enabled;
            dictionary.adaptive_ranking_enabled = enabled;
            Ok(true)
        })
    }

    pub fn clear_learning(&self) -> Result<(), DictionaryError> {
        self.update(true, |dictionary| {
            dictionary.learned.clear();
            dictionary.rankings.clear();
            dictionary.auto_phrases.clear();
            Ok(true)
        })
    }

    pub fn promote_if_generation(
        &self,
        generation: u64,
        events: &[PromotionEvent],
    ) -> Result<(), DictionaryError> {
        self.update(false, |dictionary| {
            if !dictionary.adaptive_ranking_enabled
                || dictionary.generation != generation
                || events.is_empty()
            {
                return Ok(false);
            }
            validate_promotions(events)?;
            for event in events {
                promote_order(&mut dictionary.rankings, event);
                // Reject an oversized intermediate state rather than allowing
                // a later event to hide a resource-limit violation.
                dictionary.validate()?;
            }
            Ok(true)
        })
    }

    pub fn commit_if_generation(
        &self,
        generation: u64,
        promotions: &[PromotionEvent],
        phrases: &[LearningEvent],
    ) -> Result<(), DictionaryError> {
        validate_commit(promotions, phrases)?;
        self.update(false, |dictionary| {
            if !dictionary.adaptive_ranking_enabled
                || dictionary.generation != generation
                || (promotions.is_empty() && phrases.is_empty())
            {
                return Ok(false);
            }
            for event in promotions {
                promote_order(&mut dictionary.rankings, event);
                dictionary.validate()?;
            }
            remember_auto_phrases(&mut dictionary.auto_phrases, phrases)?;
            dictionary.validate()?;
            Ok(true)
        })
    }

    pub fn learn_if_generation(
        &self,
        generation: u64,
        events: &[LearningEvent],
    ) -> Result<(), DictionaryError> {
        if events.len() > MAX_BATCH {
            return Err(DictionaryError::LimitExceeded);
        }
        self.update(false, |dictionary| {
            if !dictionary.learning_enabled
                || dictionary.generation != generation
                || events.is_empty()
            {
                return Ok(false);
            }
            // Validate the entire batch before changing the in-memory snapshot.
            for event in events {
                validate_pair(&event.pinyin, &event.text)?;
            }
            let mut merged: BTreeMap<_, _> = dictionary
                .learned
                .drain(..)
                .map(|e| ((e.pinyin, e.text), e.count))
                .collect();
            for event in events {
                let count = merged
                    .entry((event.pinyin.clone(), event.text.clone()))
                    .or_insert(0);
                *count = count.saturating_add(1).min(MAX_LEARNING_COUNT);
            }
            dictionary.learned = merged
                .into_iter()
                .map(|((pinyin, text), count)| LearnedEntry {
                    pinyin,
                    text,
                    count,
                })
                .collect();
            Ok(true)
        })
    }
}

fn load_path(path: &Path) -> Result<UserDictionary, DictionaryError> {
    let bytes = match read_bounded(path) {
        Ok(bytes) => bytes,
        Err(DictionaryError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(UserDictionary::default())
        }
        Err(error) => return Err(error),
    };
    let mut dictionary: UserDictionary =
        serde_json::from_slice(&bytes).map_err(|_| DictionaryError::InvalidData)?;
    match dictionary.schema_version {
        1 | 2 => {
            dictionary.schema_version = SCHEMA_VERSION;
        }
        SCHEMA_VERSION => {}
        _ => return Err(DictionaryError::UnsupportedVersion),
    }
    dictionary.validate()?;
    Ok(dictionary)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, DictionaryError> {
    let mut options = secure_options();
    options.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // A validated reader may retain the old snapshot across replacement;
        // allow deletion/rename, never concurrent writes to that snapshot.
        options.share_mode(5);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    reject_metadata(&metadata)?;
    if !metadata.is_file() {
        return Err(DictionaryError::UnsafePath);
    }
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err(DictionaryError::LimitExceeded);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(DictionaryError::LimitExceeded);
    }
    Ok(bytes)
}

fn secure_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Never follow a final-component reparse point. Handles deny concurrent
        // writes/deletion; the lock additionally denies all sharing.
        options.share_mode(1).custom_flags(0x0020_0000);
    }
    options
}

fn reject_metadata(metadata: &fs::Metadata) -> Result<(), DictionaryError> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(DictionaryError::UnsafePath);
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(DictionaryError::UnsafePath);
    }
    Ok(())
}

struct CheckedPath {
    path: PathBuf,
    // Deny deletion/renaming of existing Windows ancestors for the entire I/O
    // operation, not just while inspecting their reparse attributes.
    #[cfg(target_os = "windows")]
    _directories: Vec<File>,
}

impl std::ops::Deref for CheckedPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for CheckedPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

fn create_parent(parent: &Path) -> Result<Vec<CheckedPath>, DictionaryError> {
    let mut ancestor = PathBuf::new();
    let mut guards = Vec::new();
    for component in parent.components() {
        ancestor.push(component.as_os_str());
        if !matches!(component, Component::Normal(_)) {
            continue;
        }
        // Create one component at a time, holding all already checked parents.
        match fs::create_dir(&ancestor) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let guard = checked_path(&ancestor)?;
        if !fs::metadata(&ancestor)?.is_dir() {
            return Err(DictionaryError::UnsafePath);
        }
        guards.push(guard);
    }
    Ok(guards)
}

fn checked_path(path: &Path) -> Result<CheckedPath, DictionaryError> {
    if path.as_os_str().is_empty() {
        return Err(DictionaryError::UnsafePath);
    }
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.components().any(|c| matches!(c, Component::ParentDir)) || path.file_name().is_none() {
        return Err(DictionaryError::UnsafePath);
    }
    #[cfg(target_os = "windows")]
    {
        use std::path::Prefix;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        let drive = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(drive) => drive,
                _ => return Err(DictionaryError::UnsafePath),
            },
            _ => return Err(DictionaryError::UnsafePath),
        };
        if path.components().any(|c| match c {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                let stem = name
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
                    || ["COM", "LPT"].iter().any(|prefix| {
                        stem.strip_prefix(*prefix).is_some_and(|suffix| {
                            matches!(
                                suffix,
                                "1" | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³"
                            )
                        })
                    });
                reserved
                    || name.chars().any(|c| {
                        c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*')
                    })
                    || name.ends_with([' ', '.'])
            }
            _ => false,
        }) {
            return Err(DictionaryError::UnsafePath);
        }
        let root = [drive as u16, ':' as u16, '\\' as u16, 0];
        // SAFETY: root is a live, null-terminated drive-root string.
        let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
        // Fixed and removable local disks only; mapped network drives fail.
        if kind != 2 && kind != 3 {
            return Err(DictionaryError::UnsafePath);
        }
    }
    let mut ancestor = PathBuf::new();
    #[cfg(target_os = "windows")]
    let mut directories = Vec::new();
    for component in path.components() {
        ancestor.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&ancestor) {
            Ok(metadata) => {
                reject_metadata(&metadata)?;
                #[cfg(target_os = "windows")]
                if metadata.is_dir() {
                    use std::os::windows::fs::OpenOptionsExt;
                    let file = OpenOptions::new()
                        .read(true)
                        .share_mode(3)
                        .custom_flags(0x0220_0000)
                        .open(&ancestor)?;
                    reject_metadata(&file.metadata()?)?;
                    directories.push(file);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(CheckedPath {
        path,
        #[cfg(target_os = "windows")]
        _directories: directories,
    })
}

struct StoreLock {
    file: Option<File>,
    #[cfg(target_os = "windows")]
    _mutex: NamedMutex,
    #[cfg(not(target_os = "windows"))]
    path: PathBuf,
}

impl StoreLock {
    fn acquire(path: &Path) -> Result<Self, DictionaryError> {
        #[cfg(target_os = "windows")]
        let deadline = Instant::now() + Duration::from_secs(10);
        #[cfg(not(target_os = "windows"))]
        let deadline = Instant::now() + Duration::from_secs(2);
        #[cfg(target_os = "windows")]
        let mutex = NamedMutex::acquire(path, deadline)?;
        let mut name = path.as_os_str().to_os_string();
        name.push(".lock");
        let path = PathBuf::from(name);
        loop {
            if Instant::now() >= deadline {
                return Err(DictionaryError::LockTimeout);
            }
            checked_path(&path)?;
            let mut options = secure_options();
            options.read(true).write(true);
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.create(true).truncate(false).share_mode(0);
            }
            #[cfg(not(target_os = "windows"))]
            options.create_new(true);
            match options.open(&path) {
                Ok(file) => {
                    reject_metadata(&file.metadata()?)?;
                    return Ok(Self {
                        file: Some(file),
                        #[cfg(target_os = "windows")]
                        _mutex: mutex,
                        #[cfg(not(target_os = "windows"))]
                        path,
                    });
                }
                Err(error) => {
                    #[cfg(target_os = "windows")]
                    let busy = error.raw_os_error() == Some(32);
                    #[cfg(not(target_os = "windows"))]
                    let busy = error.kind() == io::ErrorKind::AlreadyExists;
                    if !busy {
                        return Err(error.into());
                    }
                    if Instant::now() >= deadline {
                        return Err(DictionaryError::LockTimeout);
                    }
                    thread::sleep(
                        Duration::from_millis(10)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
            }
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        // Close the authoritative file lock before the named mutex field is
        // dropped. No waiter can enter while our file lock is still held.
        drop(self.file.take());
        #[cfg(not(target_os = "windows"))]
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "windows")]
struct NamedMutex {
    handle: windows::Win32::Foundation::HANDLE,
    owned: bool,
}

#[cfg(target_os = "windows")]
impl NamedMutex {
    fn acquire(path: &Path, deadline: Instant) -> Result<Self, DictionaryError> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        let name = mutex_name(path)?;
        let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: No custom security descriptor, no initial ownership; the
        // null-terminated name buffer remains alive throughout the call.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
            .map_err(|_| io::Error::other("user dictionary mutex creation failed"))?;
        let mut mutex = Self {
            handle,
            owned: false,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let milliseconds = remaining.as_millis().min(u32::MAX as u128) as u32;
        // SAFETY: This is a live mutex handle owned by this RAII guard; the
        // finite timeout uses the same deadline as the file-lock fallback.
        match unsafe { WaitForSingleObject(handle, milliseconds) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                mutex.owned = true;
                Ok(mutex)
            }
            WAIT_TIMEOUT => Err(DictionaryError::LockTimeout),
            _ => Err(io::Error::last_os_error().into()),
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: A successful wait (including abandonment) gives this thread
        // ownership. Every created handle is closed exactly once, including
        // timeout/error paths where ownership was never acquired.
        unsafe {
            if self.owned {
                let _ = windows::Win32::System::Threading::ReleaseMutex(self.handle);
            }
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(target_os = "windows")]
fn mutex_name(path: &Path) -> Result<String, DictionaryError> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Prefix;
    // `checked_path` has already rejected network/device paths and `..`.
    // Rebuild components to unify slash direction and redundant `.`; fold
    // case before fixed FNV-1a over little-endian UTF-16 code units. Short
    // names/other aliases still serialize through the authoritative file lock.
    if !path.is_absolute()
        || !matches!(path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
    {
        return Err(DictionaryError::UnsafePath);
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::CurDir) {
            continue;
        }
        if matches!(component, Component::ParentDir) {
            return Err(DictionaryError::UnsafePath);
        }
        normalized.push(component.as_os_str());
    }
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    // `to_lowercase` on a valid string avoids process-dependent hash seeds.
    // Preserve non-Unicode Windows paths verbatim instead of lossy conversion;
    // fold ASCII code units there, which includes the drive and separators.
    let units: Vec<u16> = match normalized.to_str() {
        Some(value) => value.to_lowercase().encode_utf16().collect(),
        None => normalized
            .as_os_str()
            .encode_wide()
            .map(|unit| {
                if (b'A' as u16..=b'Z' as u16).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            })
            .collect(),
    };
    for unit in units {
        for byte in unit.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(format!("Global\\FlowerDictionary-{hash:016x}"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DictionaryError> {
    checked_path(path)?;
    for _ in 0..128 {
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = PathBuf::from(name);
        let mut file = match secure_options()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            publish_temporary(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
    Err(DictionaryError::LimitExceeded)
}

#[cfg(not(target_os = "windows"))]
fn publish_temporary(temporary: &Path, path: &Path) -> Result<(), DictionaryError> {
    let _target = checked_path(path)?;
    replace_file(temporary, path)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn publish_temporary(temporary: &Path, path: &Path) -> Result<(), DictionaryError> {
    // The payload was written and synced once and its write handle is closed.
    // Retain the caller's StoreLock and ancestor guards for every attempt.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _target = checked_path(path)?;
        match replace_file(temporary, path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !matches!(error.raw_os_error(), Some(5 | 32 | 33)) || Instant::now() >= deadline
                {
                    return Err(error.into());
                }
                thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
                if Instant::now() >= deadline {
                    return Err(error.into());
                }
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(target_os = "windows")]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 buffers are null-terminated and live through the
    // call. The temporary is on the same volume in the target directory.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| {
        // Preserve the Windows failure code for diagnostics without including
        // paths, entries, or input in the public error message.
        let code = error.code().0 as u32;
        if code & 0xffff_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((code & 0xffff) as i32)
        } else {
            io::Error::other("user dictionary replacement failed")
        }
    })
}

#[cfg(target_os = "windows")]
fn current_user_path() -> Result<PathBuf, DictionaryError> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    // SAFETY: Valid static folder ID, current-user token; no COM initialization
    // or COM object is needed. The shell allocation is freed below.
    let pointer =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, HANDLE::default()) }
            .map_err(|_| DictionaryError::UnsafePath)?;
    // SAFETY: The shell returned a valid, null-terminated UTF-16 allocation.
    let folder = std::ffi::OsString::from_wide(unsafe { pointer.as_wide() });
    // SAFETY: This allocation belongs to this call and is freed exactly once.
    unsafe {
        windows::Win32::System::Com::CoTaskMemFree(Some(pointer.0.cast()));
    }
    Ok(PathBuf::from(folder).join("Flower").join("dictionary.json"))
}

#[cfg(not(target_os = "windows"))]
fn current_user_path() -> Result<PathBuf, DictionaryError> {
    Err(DictionaryError::UnsupportedPlatform)
}

enum WorkerMessage {
    Learn(Vec<LearningEvent>),
    Promote(Vec<PromotionEvent>),
    Commit(Vec<PromotionEvent>, Vec<LearningEvent>),
    Stop,
}

pub struct LearningWorker {
    sender: Option<SyncSender<WorkerMessage>>,
    thread: Option<JoinHandle<bool>>,
    failed: bool,
}

impl LearningWorker {
    pub fn start(store: DictionaryStore, generation: u64) -> Result<Self, DictionaryError> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let thread = thread::Builder::new()
            .name("flower-dictionary".into())
            .spawn(move || {
                let mut failed = false;
                while let Ok(message) = receiver.recv() {
                    match message {
                        WorkerMessage::Commit(promotions, phrases) => {
                            if !failed
                                && store
                                    .commit_if_generation(generation, &promotions, &phrases)
                                    .is_err()
                            {
                                failed = true;
                            }
                        }
                        WorkerMessage::Stop => break,
                        WorkerMessage::Learn(events) => {
                            if !failed && store.learn_if_generation(generation, &events).is_err() {
                                failed = true;
                            }
                        }
                        WorkerMessage::Promote(events) => {
                            if !failed && store.promote_if_generation(generation, &events).is_err()
                            {
                                failed = true;
                            }
                        }
                    }
                }
                failed
            })?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            failed: false,
        })
    }

    /// Nonblocking and bounded in both queued batches and events per batch.
    pub fn submit(&self, events: Vec<LearningEvent>) -> Result<(), DictionaryError> {
        if events.len() > MAX_BATCH {
            return Err(DictionaryError::LimitExceeded);
        }
        self.sender
            .as_ref()
            .ok_or(DictionaryError::WorkerStopped)?
            .try_send(WorkerMessage::Learn(events))
            .map_err(|error| match error {
                TrySendError::Full(_) => DictionaryError::QueueFull,
                TrySendError::Disconnected(_) => DictionaryError::WorkerStopped,
            })
    }

    pub fn submit_promotions(&self, events: &[PromotionEvent]) -> Result<(), DictionaryError> {
        validate_promotions(events)?;
        self.sender
            .as_ref()
            .ok_or(DictionaryError::WorkerStopped)?
            .try_send(WorkerMessage::Promote(events.to_vec()))
            .map_err(|error| match error {
                TrySendError::Full(_) => DictionaryError::QueueFull,
                TrySendError::Disconnected(_) => DictionaryError::WorkerStopped,
            })
    }

    pub fn submit_commit(
        &self,
        promotions: &[PromotionEvent],
        phrases: &[LearningEvent],
    ) -> Result<(), DictionaryError> {
        validate_commit(promotions, phrases)?;
        self.sender
            .as_ref()
            .ok_or(DictionaryError::WorkerStopped)?
            .try_send(WorkerMessage::Commit(promotions.to_vec(), phrases.to_vec()))
            .map_err(|error| match error {
                TrySendError::Full(_) => DictionaryError::QueueFull,
                TrySendError::Disconnected(_) => DictionaryError::WorkerStopped,
            })
    }

    /// Drain accepted events, send stop, then join; repeated shutdown is safe.
    pub fn shutdown(&mut self) -> Result<(), DictionaryError> {
        if let Some(sender) = self.sender.take() {
            if sender.send(WorkerMessage::Stop).is_err() {
                self.failed = true;
            }
        }
        if let Some(thread) = self.thread.take() {
            self.failed |= thread.join().unwrap_or(true);
        }
        if self.failed {
            Err(DictionaryError::WorkerFailed)
        } else {
            Ok(())
        }
    }
}

impl Drop for LearningWorker {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

//! Version 2 safe, little-endian column index. Header is 208 bytes:
//! magic/version/length, label width, legacy table counts, then 20 fixed
//! (absolute offset, element count) descriptors. Columns: entry text offset,
//! text length, reading offset, reading length, weight, everyday byte;
//! exact and initial key offset/length/word start/countFlag; node childStart
//! (with sentinel), word start/countFlag; syllable offset/length; edge labels.
//! countFlag high bit denotes contiguous IDs; low 31 bits are count. Explicit
//! starts are absolute byte offsets, range starts are entry IDs. Edge ordinal
//! i always targets BFS node i+1. Labels are u16 or u32, selected in header.
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const FLOOR: u32 = 16384;
const MAGIC: &[u8; 8] = b"FLWRIDX\0";
const HEADER: usize = 208;
const RANGE: u32 = 1 << 31;

#[cfg(test)]
pub mod metrics {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Reads {
        pub words: usize,
        pub edges: usize,
        pub probes: usize,
    }
    std::thread_local! { static READS: std::cell::Cell<Reads> = const { std::cell::Cell::new(Reads { words: 0, edges: 0, probes: 0 }) }; }
    pub fn reset() {
        READS.with(|v| v.set(Reads::default()));
    }
    pub fn get() -> Reads {
        READS.with(std::cell::Cell::get)
    }
    pub(super) fn word() {
        READS.with(|v| {
            let mut n = v.get();
            n.words += 1;
            v.set(n);
        });
    }
    pub(super) fn edge() {
        READS.with(|v| {
            let mut n = v.get();
            n.edges += 1;
            v.set(n);
        });
    }
    pub(super) fn probe() {
        READS.with(|v| {
            let mut n = v.get();
            n.probes += 1;
            v.set(n);
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub weight: u32,
    pub reading: String,
    pub everyday: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryRef<'a> {
    pub text: &'a str,
    pub weight: u32,
    pub reading: &'a str,
    pub everyday: bool,
}
impl Entry {
    pub fn view(&self) -> EntryRef<'_> {
        #[cfg(test)]
        metrics::word();
        EntryRef {
            text: &self.text,
            weight: self.weight,
            reading: &self.reading,
            everyday: self.everyday,
        }
    }
}
impl EntryRef<'_> {
    pub fn owned(self) -> Entry {
        Entry {
            text: self.text.into(),
            reading: self.reading.into(),
            weight: self.weight,
            everyday: self.everyday,
        }
    }
}
pub fn rank(a: &EntryRef<'_>, b: &EntryRef<'_>) -> std::cmp::Ordering {
    let score = |e: &EntryRef<'_>| {
        if e.everyday {
            e.weight.max(FLOOR)
        } else {
            e.weight
        }
    };
    score(b)
        .cmp(&score(a))
        .then_with(|| b.weight.cmp(&a.weight))
        .then_with(|| a.text.cmp(b.text))
        .then_with(|| a.reading.cmp(b.reading))
}
pub fn rank_owned(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    rank(&a.view(), &b.view())
}

#[derive(Debug)]
pub enum ParseError {
    Empty,
    Line(usize, &'static str),
}
pub fn parse(tsv: &str) -> Result<HashMap<String, Vec<Entry>>, ParseError> {
    let mut entries: HashMap<String, Vec<Entry>> = HashMap::new();
    for (i, line) in tsv
        .strip_prefix('\u{feff}')
        .unwrap_or(tsv)
        .lines()
        .enumerate()
    {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim().starts_with('#') {
            continue;
        }
        let error = |s| ParseError::Line(i + 1, s);
        let mut fields = line.split('\t');
        let key = fields.next().unwrap_or_default().trim();
        let text = fields
            .next()
            .ok_or_else(|| error("expected pinyin, text, and weight fields"))?
            .trim();
        let weight = fields
            .next()
            .ok_or_else(|| error("missing weight field"))?
            .trim();
        if fields.next().is_some() {
            return Err(error("expected exactly three tab-separated fields"));
        }
        if key.is_empty()
            || key.len() > 256
            || !key
                .split('\'')
                .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
        {
            return Err(error(
                "pinyin must contain lowercase ASCII syllables separated by apostrophes",
            ));
        }
        if text.is_empty() {
            return Err(error("candidate text must not be empty"));
        }
        if text.chars().any(char::is_control) {
            return Err(error("candidate text must not contain control characters"));
        }
        let weight: u32 = weight
            .parse()
            .map_err(|_| error("weight must be an unsigned 32-bit integer"))?;
        if weight == 0 {
            return Err(error("weight must be greater than zero"));
        }
        entries
            .entry(key.replace('\'', ""))
            .or_default()
            .push(Entry {
                text: text.into(),
                reading: key.into(),
                weight,
                everyday: false,
            });
    }
    if entries.is_empty() {
        return Err(ParseError::Empty);
    }
    for words in entries.values_mut() {
        words.sort_by(|a, b| {
            a.text
                .cmp(&b.text)
                .then_with(|| a.reading.cmp(&b.reading))
                .then_with(|| b.weight.cmp(&a.weight))
        });
        words.dedup_by(|a, b| a.text == b.text && a.reading == b.reading);
        words.sort_by(rank_owned);
    }
    Ok(entries)
}
pub fn classify(entries: &mut HashMap<String, Vec<Entry>>, pairs: &str, phrases: &str) {
    let words: std::collections::HashSet<_> = pairs
        .split_whitespace()
        .filter_map(|p| p.split_once('='))
        .collect();
    let phrases: std::collections::HashSet<_> = phrases
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?, f.next()?))
        })
        .collect();
    for candidates in entries.values_mut() {
        for e in candidates.iter_mut() {
            let n = e.reading.split('\'').count();
            e.everyday = n >= 2
                && n == e.text.chars().count()
                && (words.contains(&(e.reading.as_str(), e.text.as_str()))
                    || phrases.contains(&(e.reading.as_str(), e.text.as_str())));
        }
        candidates.sort_by(rank_owned);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Index<'a> {
    bytes: &'a [u8],
}
impl<'a> Index<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < HEADER || bytes.get(..8)? != MAGIC {
            return None;
        }
        let this = Self { bytes };
        if this.u32(8)? != 2
            || this.n(12) != bytes.len()
            || [24, 32, 40].iter().any(|at| this.n(*at) != 0)
        {
            return None;
        }
        let width = this.n(16);
        if width != 2 && width != 4 {
            return None;
        }
        let e = this.n(20);
        let k = this.n(28);
        let i = this.n(36);
        let n = this.n(44);
        if n == 0 {
            return None;
        }
        for col in 0..20 {
            let count = this.n(52 + col * 8);
            let expected = match col {
                0..=5 => e,
                6..=9 => k,
                10..=13 => i,
                14 => n.checked_add(1)?,
                15..=16 => n,
                19 => n - 1,
                _ => count,
            };
            if count != expected {
                return None;
            }
            let w = match col {
                5 => 1,
                19 => width,
                _ => 4,
            };
            let start = this.n(48 + col * 8);
            if start < HEADER {
                return None;
            }
            this.range(start, count.checked_mul(w)?)?;
        }
        if this.n(52 + 17 * 8) != this.n(52 + 18 * 8) {
            return None;
        }
        Some(this)
    }
    fn range(self, start: usize, len: usize) -> Option<&'a [u8]> {
        self.bytes.get(start..start.checked_add(len)?)
    }
    fn u32(self, at: usize) -> Option<u32> {
        Some(u32::from_le_bytes(self.range(at, 4)?.try_into().ok()?))
    }
    fn n(self, at: usize) -> usize {
        self.u32(at).expect("validated integer") as usize
    }
    fn col(self, col: usize, id: usize) -> usize {
        assert!(id < self.n(52 + col * 8), "column ID");
        self.n(self
            .n(48 + col * 8)
            .checked_add(id.checked_mul(4).expect("column offset"))
            .expect("column offset"))
    }
    fn text(self, col: usize, id: usize) -> &'a str {
        std::str::from_utf8(
            self.range(self.col(col, id), self.col(col + 1, id))
                .expect("string range"),
        )
        .expect("UTF-8")
    }
    fn words(self, col: usize, id: usize) -> Words<'a> {
        let start = self.col(col, id);
        let flag = self.col(col + 1, id) as u32;
        let count = (flag & !RANGE) as usize;
        if flag & RANGE != 0 {
            assert!(
                start <= self.n(20) && count <= self.n(20) - start,
                "range IDs"
            );
            Words::Range(self, start, count)
        } else {
            Words::Indexed(
                self,
                self.range(start, count.checked_mul(4).expect("ID size"))
                    .expect("ID range"),
            )
        }
    }
    pub fn entry(self, id: usize) -> EntryRef<'a> {
        #[cfg(test)]
        metrics::word();
        assert!(id < self.n(20));
        let everyday = self
            .range(self.n(48 + 5 * 8).checked_add(id).expect("flag offset"), 1)
            .expect("flag")[0];
        assert!(everyday <= 1, "everyday flag");
        EntryRef {
            text: self.text(0, id),
            reading: self.text(2, id),
            weight: self.col(4, id) as u32,
            everyday: everyday != 0,
        }
    }
    fn key_col(slot: usize) -> usize {
        match slot {
            24 => 6,
            32 => 10,
            _ => panic!("table slot"),
        }
    }
    fn keys(self, slot: usize) -> impl Iterator<Item = &'a str> + 'a {
        (0..self.n(slot + 4)).map(move |id| self.text(Self::key_col(slot), id))
    }
    fn get(self, slot: usize, key: &str) -> Option<Words<'a>> {
        let col = Self::key_col(slot);
        let (mut low, mut high) = (0, self.n(slot + 4));
        while low < high {
            let mid = low + (high - low) / 2;
            match self.text(col, mid).cmp(key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(self.words(col + 2, mid)),
            }
        }
        None
    }
    fn label(self, edge: usize) -> usize {
        assert!(edge < self.n(44) - 1);
        let width = self.n(16);
        let at = self
            .n(48 + 19 * 8)
            .checked_add(edge.checked_mul(width).expect("label offset"))
            .expect("label offset");
        let label = if width == 2 {
            u16::from_le_bytes(self.range(at, 2).expect("label").try_into().unwrap()) as usize
        } else {
            self.n(at)
        };
        assert!(label < self.n(52 + 17 * 8));
        label
    }
    fn syllable_first(self, edge: usize) -> u8 {
        let label = self.label(edge);
        // Group probes need only the first byte, not a decoded syllable.
        // Validate the entire range and reject empty strings here; full UTF-8
        // validation remains in indexed_edge when the edge is consumed.
        *self
            .range(self.col(17, label), self.col(18, label))
            .expect("syllable range")
            .first()
            .expect("nonempty syllable")
    }
    fn node(self, id: usize) -> NodeView<'a> {
        assert!(id < self.n(44));
        let start = self.col(14, id);
        let end = self.col(14, id + 1);
        assert!(start <= end && end < self.n(44), "edge bounds");
        NodeView {
            edges: Edges::Indexed(self, start..end),
            words: self.words(15, id),
        }
    }
}

/// Replacements contain complete buckets only for touched keys. Cloning a
/// snapshot copies this small patch, never the immutable base tables.
#[derive(Debug, Clone)]
pub struct Table {
    base: Option<Index<'static>>,
    dynamic: Option<Arc<HashMap<String, Vec<Entry>>>>,
    slot: usize,
    patch: HashMap<String, Vec<Entry>>,
}
impl Table {
    #[cfg(test)]
    pub fn pristine_index(&self) -> bool {
        self.base.is_some() && self.dynamic.is_none() && self.patch.is_empty()
    }
    #[cfg(test)]
    pub fn patch_len(&self) -> usize {
        self.patch.len()
    }
    pub fn dynamic(entries: HashMap<String, Vec<Entry>>) -> Self {
        Self {
            base: None,
            dynamic: Some(Arc::new(entries)),
            slot: 24,
            patch: HashMap::new(),
        }
    }
    pub fn indexed(base: Index<'static>, slot: usize) -> Self {
        Self {
            base: Some(base),
            dynamic: None,
            slot,
            patch: HashMap::new(),
        }
    }
    pub fn get(&self, key: &str) -> Option<Words<'_>> {
        self.patch
            .get(key)
            .or_else(|| self.dynamic.as_ref().and_then(|b| b.get(key)))
            .map(|v| Words::Dynamic(v))
            .or_else(|| self.base.and_then(|b| b.get(self.slot, key)))
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.patch.keys().map(String::as_str).chain(
            self.base
                .into_iter()
                .flat_map(move |b| b.keys(self.slot))
                .chain(
                    self.dynamic
                        .iter()
                        .flat_map(|b| b.keys().map(String::as_str)),
                )
                .filter(|k| !self.patch.contains_key(*k)),
        )
    }
    pub fn values(&self) -> impl Iterator<Item = Words<'_>> {
        self.iter().map(|(_, words)| words)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, Words<'_>)> {
        self.patch
            .iter()
            .map(|(key, words)| (key.as_str(), Words::Dynamic(words.as_slice())))
            .chain(
                self.dynamic
                    .iter()
                    .flat_map(|b| b.iter())
                    .filter(|(key, _)| !self.patch.contains_key(key.as_str()))
                    .map(|(key, words)| (key.as_str(), Words::Dynamic(words.as_slice()))),
            )
            .chain(
                self.base
                    .into_iter()
                    .flat_map(move |b| {
                        (0..b.n(self.slot + 4)).map(move |id| {
                            let col = Index::key_col(self.slot);
                            (b.text(col, id), b.words(col + 2, id))
                        })
                    })
                    .filter(|(key, _)| !self.patch.contains_key(*key)),
            )
    }
    pub fn len(&self) -> usize {
        self.base.map_or(0, |b| b.n(self.slot + 4))
            + self.dynamic.as_ref().map_or(0, |b| b.len())
            + self
                .patch
                .keys()
                .filter(|k| {
                    self.base.map_or(true, |b| b.get(self.slot, k).is_none())
                        && self.dynamic.as_ref().map_or(true, |b| !b.contains_key(*k))
                })
                .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn replace(&mut self, key: String, words: Vec<Entry>) {
        self.patch.insert(key, words);
    }
}
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub edges: BTreeMap<u8, Vec<(String, usize)>>,
    pub words: Vec<Entry>,
}
#[derive(Debug)]
pub struct NodeView<'a> {
    pub edges: Edges<'a>,
    pub words: Words<'a>,
}
/// Borrowed ordered edges: opening a node does not copy a map or decode any
/// edge. Indexed initial-group selection probes only first bytes with binary
/// search. Full syllables and destinations are decoded on iterator demand.
#[derive(Debug, Clone)]
pub enum Edges<'a> {
    Dynamic(&'a BTreeMap<u8, Vec<(String, usize)>>),
    Indexed(Index<'a>, std::ops::Range<usize>),
}
pub enum EdgeIter<'a> {
    Dynamic(std::iter::Flatten<std::collections::btree_map::Values<'a, u8, Vec<(String, usize)>>>),
    Group(std::slice::Iter<'a, (String, usize)>),
    Indexed(Index<'a>, std::ops::Range<usize>),
}
impl<'a> Edges<'a> {
    pub fn len(&self) -> usize {
        match self {
            Self::Dynamic(map) => map.values().map(Vec::len).sum(),
            Self::Indexed(_, bytes) => bytes.len(),
        }
    }
    pub fn iter(&self) -> EdgeIter<'a> {
        match self.clone() {
            Self::Dynamic(map) => EdgeIter::Dynamic(map.values().flatten()),
            Self::Indexed(index, bytes) => EdgeIter::Indexed(index, bytes),
        }
    }
    pub fn group(&self, first: u8) -> EdgeIter<'a> {
        match self.clone() {
            Self::Dynamic(map) => {
                EdgeIter::Group(map.get(&first).map_or(&[][..], Vec::as_slice).iter())
            }
            Self::Indexed(index, bytes) => {
                let boundary = |upper: bool| {
                    let (mut low, mut high) = (bytes.start, bytes.end);
                    while low < high {
                        #[cfg(test)]
                        metrics::probe();
                        let mid = low + (high - low) / 2;
                        let byte = index.syllable_first(mid);
                        if byte < first || (upper && byte == first) {
                            low = mid + 1;
                        } else {
                            high = mid;
                        }
                    }
                    low
                };
                let start = boundary(false);
                let end = boundary(true);
                EdgeIter::Indexed(index, start..end)
            }
        }
    }
}
fn dynamic_edge(edge: &(String, usize)) -> (&str, usize) {
    #[cfg(test)]
    metrics::edge();
    (edge.0.as_str(), edge.1)
}
fn indexed_edge<'a>(index: Index<'a>, ordinal: usize) -> (&'a str, usize) {
    #[cfg(test)]
    metrics::edge();
    (index.text(17, index.label(ordinal)), ordinal + 1)
}
impl<'a> Iterator for EdgeIter<'a> {
    type Item = (&'a str, usize);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Dynamic(iter) => iter.next().map(dynamic_edge),
            Self::Group(iter) => iter.next().map(dynamic_edge),
            Self::Indexed(index, iter) => iter.next().map(|record| indexed_edge(*index, record)),
        }
    }
}
impl DoubleEndedIterator for EdgeIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Dynamic(iter) => iter.next_back().map(dynamic_edge),
            Self::Group(iter) => iter.next_back().map(dynamic_edge),
            Self::Indexed(index, iter) => {
                iter.next_back().map(|record| indexed_edge(*index, record))
            }
        }
    }
}
/// Terminal words are decoded only when the query's shared budget admits them.
/// In particular a high-fanout terminal never materializes its entire bucket
/// before the mixed/completion budget is applied.
#[derive(Debug, Clone, Copy)]
pub enum Words<'a> {
    Range(Index<'a>, usize, usize),
    Dynamic(&'a [Entry]),
    Indexed(Index<'a>, &'a [u8]),
}
impl<'a> Words<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Dynamic(v) => v.len(),
            Self::Range(_, _, n) => n,
            Self::Indexed(_, v) => v.len() / 4,
        }
    }
    pub fn iter(self) -> WordIter<'a> {
        let len = self.len();
        WordIter {
            words: self,
            next: 0,
            len,
        }
    }
    fn at(self, i: usize) -> EntryRef<'a> {
        assert!(i < self.len());
        match self {
            Self::Dynamic(v) => v[i].view(),
            Self::Range(index, start, _) => index.entry(start.checked_add(i).expect("range ID")),
            Self::Indexed(index, bytes) => index
                .entry(u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()) as usize),
        }
    }
}
pub struct WordIter<'a> {
    words: Words<'a>,
    next: usize,
    len: usize,
}
impl<'a> Iterator for WordIter<'a> {
    type Item = EntryRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.len {
            return None;
        }
        let i = self.next;
        self.next += 1;
        Some(self.words.at(i))
    }
}
impl DoubleEndedIterator for WordIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.next == self.len {
            return None;
        }
        self.len -= 1;
        Some(self.words.at(self.len))
    }
}
impl<'a> IntoIterator for Words<'a> {
    type Item = EntryRef<'a>;
    type IntoIter = WordIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl Node {
    fn view(&self) -> NodeView<'_> {
        NodeView {
            edges: Edges::Dynamic(&self.edges),
            words: Words::Dynamic(&self.words),
        }
    }
}
impl NodeView<'_> {
    fn owned(self) -> Node {
        let mut edges: BTreeMap<u8, Vec<(String, usize)>> = BTreeMap::new();
        for (syllable, next) in self.edges.iter() {
            edges
                .entry(syllable.as_bytes()[0])
                .or_default()
                .push((syllable.into(), next));
        }
        Node {
            edges,
            words: self.words.into_iter().map(EntryRef::owned).collect(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Trie {
    base: Option<Index<'static>>,
    dynamic: Option<Arc<Vec<Node>>>,
    patch: HashMap<usize, Arc<Node>>,
    count: usize,
}
impl Trie {
    #[cfg(test)]
    pub fn pristine_index(&self) -> bool {
        self.base.is_some() && self.dynamic.is_none() && self.patch.is_empty()
    }
    pub fn indexed(base: Index<'static>) -> Self {
        Self {
            base: Some(base),
            dynamic: None,
            patch: HashMap::new(),
            count: base.n(44),
        }
    }
    pub fn node(&self, id: usize) -> NodeView<'_> {
        self.patch
            .get(&id)
            .map(|n| n.view())
            .or_else(|| {
                self.dynamic
                    .as_ref()
                    .and_then(|b| b.get(id))
                    .map(Node::view)
            })
            .unwrap_or_else(|| self.base.expect("dynamic node exists").node(id))
    }
    pub fn len(&self) -> usize {
        self.count
    }
    fn node_mut(&mut self, id: usize) -> &mut Node {
        if !self.patch.contains_key(&id) {
            let node = self.node(id).owned();
            self.patch.insert(id, Arc::new(node));
        }
        Arc::make_mut(self.patch.get_mut(&id).unwrap())
    }
    pub fn build(entries: &HashMap<String, Vec<Entry>>) -> Self {
        let mut nodes = vec![Node::default()];
        let mut words: Vec<_> = entries.values().flatten().collect();
        words.sort_by(|a, b| a.reading.cmp(&b.reading).then_with(|| rank_owned(a, b)));
        for word in words {
            let syllables: Vec<_> = word.reading.split('\'').collect();
            if syllables.len() < 2 || syllables.len() != word.text.chars().count() {
                continue;
            }
            let mut node = 0;
            for syllable in syllables {
                let first = syllable.as_bytes()[0];
                let next = nodes[node]
                    .edges
                    .get(&first)
                    .and_then(|v| v.iter().find(|(s, _)| s == syllable))
                    .map(|(_, n)| *n);
                node = match next {
                    Some(n) => n,
                    None => {
                        let n = nodes.len();
                        nodes.push(Node::default());
                        nodes[node]
                            .edges
                            .entry(first)
                            .or_default()
                            .push((syllable.into(), n));
                        n
                    }
                };
            }
            nodes[node].words.push(word.clone());
        }
        for node in &mut nodes {
            node.words.sort_by(rank_owned);
        }
        Self {
            base: None,
            count: nodes.len(),
            dynamic: Some(Arc::new(nodes)),
            patch: HashMap::new(),
        }
    }
    pub fn put(&mut self, word: Entry) {
        let syllables: Vec<_> = word.reading.split('\'').collect();
        if syllables.len() < 2 || syllables.len() != word.text.chars().count() {
            return;
        }
        let mut node = 0;
        for syllable in syllables {
            let first = syllable.as_bytes()[0];
            let next = self
                .node(node)
                .edges
                .group(first)
                .find(|(s, _)| *s == syllable)
                .map(|(_, n)| n);
            node = match next {
                Some(n) => n,
                None => {
                    let n = self.count;
                    self.count += 1;
                    self.patch.insert(n, Arc::new(Node::default()));
                    let edges = self.node_mut(node).edges.entry(first).or_default();
                    edges.push((syllable.into(), n));
                    edges.sort_by(|a, b| a.0.cmp(&b.0));
                    n
                }
            };
        }
        let words = &mut self.node_mut(node).words;
        if let Some(old) = words
            .iter_mut()
            .find(|e| e.text == word.text && e.reading == word.reading)
        {
            *old = word;
        } else {
            words.push(word);
        }
        words.sort_by(rank_owned);
    }
}
pub fn initials(entries: &HashMap<String, Vec<Entry>>) -> HashMap<String, Vec<Entry>> {
    let mut result: HashMap<String, Vec<Entry>> = HashMap::new();
    for e in entries.values().flatten() {
        let syllables: Vec<_> = e.reading.split('\'').collect();
        if syllables.len() == e.text.chars().count() {
            result
                .entry(syllables.iter().map(|s| s.as_bytes()[0] as char).collect())
                .or_default()
                .push(e.clone());
        }
    }
    for words in result.values_mut() {
        words.sort_by(rank_owned);
    }
    result
}

fn num(n: usize) -> u32 {
    u32::try_from(n).expect("builtin index exceeds u32 format limit")
}
fn put(bytes: &mut [u8], at: usize, n: usize) {
    bytes[at..at + 4].copy_from_slice(&num(n).to_le_bytes());
}
fn column(bytes: &mut Vec<u8>, col: usize, values: &[u32], width: usize) {
    let start = bytes.len();
    put(bytes, 48 + col * 8, start);
    put(bytes, 52 + col * 8, values.len());
    for value in values {
        match width {
            1 => bytes.push(u8::try_from(*value).expect("byte")),
            2 => bytes.extend_from_slice(&u16::try_from(*value).expect("label").to_le_bytes()),
            4 => bytes.extend_from_slice(&value.to_le_bytes()),
            _ => unreachable!(),
        }
    }
}
fn strings(bytes: &mut Vec<u8>, col: usize, values: &[&str], shared: bool) {
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    let mut seen = HashMap::new();
    for &s in values {
        let offset = if shared {
            *seen.entry(s).or_insert_with(|| {
                let at = num(bytes.len());
                bytes.extend_from_slice(s.as_bytes());
                at
            })
        } else {
            let at = num(bytes.len());
            bytes.extend_from_slice(s.as_bytes());
            at
        };
        offsets.push(offset);
        lengths.push(num(s.len()));
    }
    column(bytes, col, &offsets, 4);
    column(bytes, col + 1, &lengths, 4);
}
fn lists(bytes: &mut Vec<u8>, col: usize, lists: impl Iterator<Item = Vec<usize>>) {
    let mut starts = Vec::new();
    let mut counts = Vec::new();
    for ids in lists {
        let count = num(ids.len());
        assert!(count < RANGE, "ID count exceeds countFlag");
        if !ids.is_empty()
            && ids
                .iter()
                .enumerate()
                .all(|(i, id)| ids[0].checked_add(i) == Some(*id))
        {
            starts.push(num(ids[0]));
            counts.push(count | RANGE);
        } else {
            starts.push(num(bytes.len()));
            counts.push(count);
            for id in ids {
                bytes.extend_from_slice(&num(id).to_le_bytes());
            }
        }
    }
    column(bytes, col, &starts, 4);
    column(bytes, col + 1, &counts, 4);
}
/// Encode deterministically, then compare every entry, bucket and BFS-mapped
/// edge destination against the independent dynamic trie oracle.
pub fn encode(entries: &HashMap<String, Vec<Entry>>) -> Vec<u8> {
    let initial = initials(entries);
    let trie = Trie::build(entries);
    let mut keys: Vec<_> = entries.keys().collect();
    keys.sort();
    let words: Vec<_> = keys.iter().flat_map(|k| entries[*k].iter()).collect();
    let ids: HashMap<_, _> = words
        .iter()
        .enumerate()
        .map(|(i, e)| ((e.reading.as_str(), e.text.as_str()), i))
        .collect();
    let mut bfs = vec![0];
    let mut mapped = vec![usize::MAX; trie.count];
    mapped[0] = 0;
    let mut cursor = 0;
    while cursor < bfs.len() {
        for (_, child) in trie.node(bfs[cursor]).edges.iter() {
            assert_eq!(mapped[child], usize::MAX, "tree child appears once");
            mapped[child] = bfs.len();
            bfs.push(child);
        }
        cursor += 1;
    }
    assert_eq!(bfs.len(), trie.count);
    let mut bytes = vec![0; HEADER];
    bytes[..8].copy_from_slice(MAGIC);
    put(&mut bytes, 8, 2);
    put(&mut bytes, 20, words.len());
    put(&mut bytes, 28, entries.len());
    put(&mut bytes, 36, initial.len());
    put(&mut bytes, 44, trie.count);
    strings(
        &mut bytes,
        0,
        &words.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
        false,
    );
    strings(
        &mut bytes,
        2,
        &words.iter().map(|e| e.reading.as_str()).collect::<Vec<_>>(),
        true,
    );
    column(
        &mut bytes,
        4,
        &words.iter().map(|e| e.weight).collect::<Vec<_>>(),
        4,
    );
    column(
        &mut bytes,
        5,
        &words
            .iter()
            .map(|e| u32::from(e.everyday))
            .collect::<Vec<_>>(),
        1,
    );
    for (col, table) in [(6, entries), (10, &initial)] {
        let mut keys: Vec<_> = table.keys().map(String::as_str).collect();
        keys.sort();
        strings(&mut bytes, col, &keys, false);
        lists(
            &mut bytes,
            col + 2,
            keys.iter().map(|k| {
                table[*k]
                    .iter()
                    .map(|e| ids[&(e.reading.as_str(), e.text.as_str())])
                    .collect()
            }),
        );
    }
    let mut syllables = Vec::new();
    let mut syllable_ids = HashMap::new();
    let mut labels = Vec::new();
    let mut starts = Vec::new();
    for &old in &bfs {
        starts.push(num(labels.len()));
        for (s, _) in trie.node(old).edges.iter() {
            let label = *syllable_ids.entry(s.to_owned()).or_insert_with(|| {
                let id = num(syllables.len());
                syllables.push(s.to_owned());
                id
            });
            labels.push(label);
        }
    }
    starts.push(num(labels.len()));
    column(&mut bytes, 14, &starts, 4);
    lists(
        &mut bytes,
        15,
        bfs.iter().map(|old| {
            trie.node(*old)
                .words
                .iter()
                .map(|e| ids[&(e.reading, e.text)])
                .collect()
        }),
    );
    strings(
        &mut bytes,
        17,
        &syllables.iter().map(String::as_str).collect::<Vec<_>>(),
        false,
    );
    let width = if syllables.len() <= 65536 { 2 } else { 4 };
    put(&mut bytes, 16, width);
    column(&mut bytes, 19, &labels, width);
    let len = bytes.len();
    put(&mut bytes, 12, len);
    let index = Index::open(&bytes).expect("encoded header");
    for (id, word) in words.iter().enumerate() {
        assert_eq!(index.entry(id), word.view(), "entry {id}");
    }
    for (slot, table) in [(24, entries), (32, &initial)] {
        assert_eq!(index.keys(slot).count(), table.len());
        for key in index.keys(slot) {
            assert!(
                index
                    .get(slot, key)
                    .unwrap()
                    .iter()
                    .eq(table[key].iter().map(Entry::view)),
                "bucket {key}"
            );
        }
    }
    for (new, &old) in bfs.iter().enumerate() {
        let a = index.node(new);
        let b = trie.node(old);
        assert!(
            a.edges
                .iter()
                .eq(b.edges.iter().map(|(s, n)| (s, mapped[n]))),
            "BFS edge {new}/{old}"
        );
        assert!(a.words.iter().eq(b.words.iter()), "BFS words {new}/{old}");
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(bytes: &mut [u8]) {
        let len = bytes.len();
        put(bytes, 12, len);
    }

    #[test]
    fn range_explicit_reverse_and_boundary_reads() {
        let entries = parse("a'a\t阿啊\t4294967295\na'a\t啊阿\t1").unwrap();
        let mut bytes = encode(&entries);
        let index = Index::open(&bytes).unwrap();
        let words = index.get(24, "aa").unwrap();
        assert!(matches!(words, Words::Range(_, 0, 2)));
        assert_eq!(words.iter().next().unwrap().weight, u32::MAX);
        assert_eq!(index.col(2, 0), index.col(2, 1), "shared reading");
        metrics::reset();
        assert_eq!(words.iter().rev().take(1).next().unwrap().text, "啊阿");
        assert_eq!(metrics::get().words, 1);
        let mut iter = words.iter();
        assert_eq!(iter.next().unwrap().text, "阿啊");
        assert_eq!(iter.next_back().unwrap().text, "啊阿");
        assert!(iter.next().is_none());
        assert!(iter.next_back().is_none());

        lists(&mut bytes, 8, std::iter::once(vec![1, 0]));
        finish(&mut bytes);
        let index = Index::open(&bytes).unwrap();
        let words = index.get(24, "aa").unwrap();
        assert!(matches!(words, Words::Indexed(..)));
        metrics::reset();
        assert_eq!(words.iter().take(0).count(), 0);
        assert_eq!(metrics::get().words, 0);
        assert_eq!(words.iter().next_back().unwrap().text, "阿啊");
        assert_eq!(metrics::get().words, 1);

        // The header never scans payload. Corruption is rejected safely on
        // bounded access rather than triggering eager startup validation.
        let start = index.n(48 + 8 * 8);
        let count = index.n(48 + 9 * 8);
        for (first, flag) in [(u32::MAX, RANGE | 2), (0, u32::MAX), (u32::MAX, 2)] {
            let mut bad = bytes.clone();
            put(&mut bad, start, first as usize);
            put(&mut bad, count, flag as usize);
            assert!(std::panic::catch_unwind(|| {
                Index::open(&bad)
                    .unwrap()
                    .get(24, "aa")
                    .unwrap()
                    .iter()
                    .next()
            })
            .is_err());
        }
        let mut bad = bytes.clone();
        let at = index.col(8, 0);
        put(&mut bad, at, u32::MAX as usize);
        assert!(std::panic::catch_unwind(|| {
            Index::open(&bad)
                .unwrap()
                .get(24, "aa")
                .unwrap()
                .iter()
                .next()
        })
        .is_err());
    }

    #[test]
    fn all_descriptors_and_payload_flags_are_checked() {
        let bytes = encode(&parse("a'a\t阿啊\t1").unwrap());
        for n in 0..bytes.len() {
            assert!(Index::open(&bytes[..n]).is_none());
        }
        for col in 0..20 {
            for field in [48 + col * 8, 52 + col * 8] {
                let mut bad = bytes.clone();
                put(&mut bad, field, u32::MAX as usize);
                assert!(Index::open(&bad).is_none(), "descriptor {col}/{field}");
            }
        }
        let index = Index::open(&bytes).unwrap();
        let mut bad = bytes.clone();
        bad[index.n(48 + 5 * 8)] = 2;
        assert!(std::panic::catch_unwind(|| Index::open(&bad).unwrap().entry(0)).is_err());
        let mut bad = bytes.clone();
        put(&mut bad, index.n(48 + 14 * 8) + 4, u32::MAX as usize);
        assert!(std::panic::catch_unwind(|| Index::open(&bad).unwrap().node(0)).is_err());
        let mut bad = bytes.clone();
        let labels = index.n(48 + 19 * 8);
        bad[labels..labels + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(std::panic::catch_unwind(|| Index::open(&bad)
            .unwrap()
            .node(0)
            .edges
            .iter()
            .next())
        .is_err());
        assert!(std::panic::catch_unwind(|| num(u32::MAX as usize + 1)).is_err());
    }

    #[test]
    fn group_probes_defer_utf8_validation_until_edge_consumption() {
        let mut bytes = encode(&parse("aa'a\t阿啊\t1\nzz'a\t杂啊\t2").unwrap());
        let index = Index::open(&bytes).unwrap();
        let label = index.label(1);
        assert_eq!(index.text(17, label), "zz");
        let offset = index.col(17, label);
        bytes[offset + 1] = 0xff;
        let index = Index::open(&bytes).unwrap();
        metrics::reset();
        // The first binary-search midpoint is the corrupted "zz" edge.
        // Its first byte can be inspected without decoding its trailing byte.
        let edges = index.node(0).edges;
        let mut a = edges.group(b'a');
        assert!(metrics::get().probes > 0);
        assert_eq!(metrics::get().edges, 0);
        assert_eq!(a.next(), Some(("aa", 1)));
        assert!(a.next().is_none());
        assert_eq!(metrics::get().edges, 1);
        metrics::reset();
        let mut z = edges.group(b'z');
        assert_eq!(metrics::get().edges, 0);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| z.next())).is_err());
        assert_eq!(metrics::get().edges, 1);
        assert!(std::panic::catch_unwind(|| edges.group(b'z').next_back()).is_err());
    }

    #[test]
    fn group_probes_reject_invalid_labels_empty_and_out_of_bounds_syllables() {
        let bytes = encode(&parse("aa'a\t阿啊\t1\nzz'a\t杂啊\t2").unwrap());
        let index = Index::open(&bytes).unwrap();
        let label = index.label(1);
        let offset_at = index.n(48 + 17 * 8) + label * 4;
        let length_at = index.n(48 + 18 * 8) + label * 4;
        let original_offset = index.col(17, label);
        for (offset, len) in [
            (original_offset, 0),
            (u32::MAX as usize, 2),
            // The first byte exists, but the complete declared range does not.
            (bytes.len() - 1, 2),
        ] {
            let mut bad = bytes.clone();
            put(&mut bad, offset_at, offset);
            put(&mut bad, length_at, len);
            assert!(std::panic::catch_unwind(|| {
                let _ = Index::open(&bad).unwrap().node(0).edges.group(b'z');
            })
            .is_err());
        }
        let mut bad = bytes.clone();
        let label_at = index.n(48 + 19 * 8) + 2;
        bad[label_at..label_at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(std::panic::catch_unwind(|| {
            let _ = Index::open(&bad).unwrap().node(0).edges.group(b'z');
        })
        .is_err());
    }

    #[test]
    fn wide_labels_preserve_ids_above_u16() {
        // Small physical fixture with a repeated string table; labels must not
        // truncate even when a legal syllable ID is larger than u16::MAX.
        let mut bytes = encode(&parse("a'a\t阿啊\t1").unwrap());
        let index = Index::open(&bytes).unwrap();
        let offset = index.col(17, 0) as u32;
        column(&mut bytes, 17, &vec![offset; 65537], 4);
        column(&mut bytes, 18, &vec![1; 65537], 4);
        column(&mut bytes, 19, &[65536, 65536], 4);
        put(&mut bytes, 16, 4);
        finish(&mut bytes);
        let index = Index::open(&bytes).unwrap();
        assert_eq!(index.label(0), 65536);
        assert_eq!(index.node(0).edges.iter().next(), Some(("a", 1)));
        assert_eq!(index.node(1).edges.iter().next_back(), Some(("a", 2)));
        assert_eq!(index.node(0).edges.group(b'a').next(), Some(("a", 1)));
    }

    #[test]
    fn indexed_patch_keeps_bfs_destinations_and_snapshot_isolation() {
        let entries = parse("a'a\t阿啊\t1\nz'a\t杂啊\t2\na'z\t阿杂\t3").unwrap();
        let bytes = Box::leak(encode(&entries).into_boxed_slice());
        let index = Index::open(bytes).unwrap();
        let base = Trie::indexed(index);
        let mut clone = base.clone();
        let replacement = Entry {
            text: "私啊".into(),
            reading: "z'a".into(),
            weight: 9,
            everyday: true,
        };
        clone.put(replacement.clone());
        let child = clone.node(0).edges.group(b'z').next().unwrap().1;
        let leaf = clone.node(child).edges.group(b'a').next().unwrap().1;
        assert!(clone
            .node(leaf)
            .words
            .iter()
            .any(|e| e == replacement.view()));
        assert!(!base
            .node(leaf)
            .words
            .iter()
            .any(|e| e == replacement.view()));
        assert_eq!(clone.patch.len(), 1);
        clone.put(Entry {
            reading: "z'z".into(),
            ..replacement.clone()
        });
        assert_eq!(clone.len(), base.len() + 1);
        assert!(base.node(child).edges.group(b'z').next().is_none());
        let table = Table::indexed(index, 24);
        let mut patched = table.clone();
        patched.replace("za".into(), vec![replacement]);
        assert_eq!(table.get("za").unwrap().iter().next().unwrap().text, "杂啊");
        assert_eq!(
            patched.get("za").unwrap().iter().next().unwrap().text,
            "私啊"
        );
        assert_eq!(patched.patch_len(), 1);
        assert!(table.pristine_index());
    }

    #[test]
    fn large_range_and_explicit_buckets_only_read_admitted_words() {
        let mut tsv = String::new();
        for id in 0..8192u32 {
            tsv.push_str(&format!(
                "ni'hao\t你{}\t{}\n",
                char::from_u32(0x4e00 + id).unwrap(),
                id + 1
            ));
        }
        let entries = parse(&tsv).unwrap();
        let mut bytes = encode(&entries);
        for explicit in [false, true] {
            if explicit {
                lists(&mut bytes, 8, std::iter::once((0..8192).rev().collect()));
                finish(&mut bytes);
            }
            let index = Index::open(&bytes).unwrap();
            for limit in [0, 1, 4] {
                metrics::reset();
                let words = index.get(24, "nihao").unwrap();
                assert_eq!(words.len(), 8192);
                assert_eq!(metrics::get().words, 0);
                assert_eq!(words.iter().take(limit).count(), limit);
                assert_eq!(metrics::get().words, limit);
                metrics::reset();
                assert_eq!(words.iter().rev().take(limit).count(), limit);
                assert_eq!(metrics::get().words, limit);
            }
        }
    }

    #[test]
    fn lazy_node_views_decode_on_demand_in_both_directions() {
        let mut tsv = String::from("a'a\t阿啊\t1\nz'a\t杂啊\t1\n");
        for i in 0..2048u32 {
            let suffix = [
                b'h',
                b'a' + (i / 676) as u8,
                b'a' + ((i / 26) % 26) as u8,
                b'a' + (i % 26) as u8,
            ];
            tsv.push_str(&format!(
                "{}'a\t你好\t1\n",
                std::str::from_utf8(&suffix).unwrap()
            ));
        }
        let entries = parse(&tsv).unwrap();
        let bytes = encode(&entries);
        let index = Index::open(&bytes).unwrap();
        let dynamic = Trie::build(&entries);
        let a = index.node(0).edges;
        let b = dynamic.node(0).edges;
        assert!(a.iter().map(|(s, _)| s).eq(b.iter().map(|(s, _)| s)));
        assert!(a
            .iter()
            .rev()
            .map(|(s, _)| s)
            .eq(b.iter().rev().map(|(s, _)| s)));
        assert!(a
            .group(b'h')
            .map(|(s, _)| s)
            .eq(b.group(b'h').map(|(s, _)| s)));
        assert!(a
            .group(b'h')
            .rev()
            .map(|(s, _)| s)
            .eq(b.group(b'h').rev().map(|(s, _)| s)));
        for edges in [a, b] {
            metrics::reset();
            assert_eq!(edges.len(), 2050);
            assert_eq!(metrics::get().edges, 0);
            assert_eq!(edges.iter().take(0).count(), 0);
            assert_eq!(metrics::get().edges, 0);
            assert_eq!(edges.group(b'h').rev().take(2).count(), 2);
            let reads = metrics::get();
            assert_eq!(reads.edges, 2);
            assert!(reads.probes <= 24, "{reads:?}");
        }
        metrics::reset();
        let _ = index.node(0);
        let _ = dynamic.node(0);
        assert_eq!(metrics::get(), metrics::Reads::default());
        let table = Table::dynamic(entries);
        metrics::reset();
        let _ = table.get("aa");
        let _ = index.get(24, "aa");
        assert_eq!(metrics::get(), metrics::Reads::default());
    }

    #[test]
    fn deterministic_round_trip_and_checked_header() {
        let a = parse("ni'hao\t你好\t10\nni\t你\t20\nni'hao\t你好\t1").unwrap();
        let b = parse("ni\t你\t20\nni'hao\t你好\t1\nni'hao\t你好\t10").unwrap();
        let bytes = encode(&a);
        assert_eq!(bytes, encode(&b));
        let index = Index::open(&bytes).unwrap();
        assert_eq!(
            index.get(24, "nihao").unwrap().iter().next().unwrap().text,
            "你好"
        );
        assert!(index.get(24, "missing").is_none());
        for n in 0..HEADER {
            assert!(Index::open(&bytes[..n]).is_none());
        }
        for at in [0, 8, 12, 16, 20, 28, 36, 44, 48, 52] {
            let mut bad = bytes.clone();
            bad[at..at + 4].fill(255);
            assert!(Index::open(&bad).is_none(), "header field {at}");
        }
    }

    #[test]
    fn dynamic_snapshot_clones_only_patches() {
        let entries = parse("ni'hao\t你好\t10\nna'li\t那里\t20").unwrap();
        let table = Table::dynamic(entries.clone());
        let clone = table.clone();
        assert!(Arc::ptr_eq(
            table.dynamic.as_ref().unwrap(),
            clone.dynamic.as_ref().unwrap()
        ));
        assert!(clone.patch.is_empty());
        let trie = Trie::build(&entries);
        let mut clone = trie.clone();
        clone.put(Entry {
            text: "私好".into(),
            reading: "ni'hao".into(),
            weight: 30,
            everyday: false,
        });
        assert!(Arc::ptr_eq(
            trie.dynamic.as_ref().unwrap(),
            clone.dynamic.as_ref().unwrap()
        ));
        assert_eq!(clone.patch.len(), 1);
        assert!(trie.patch.is_empty());
    }

    #[test]
    fn parser_dedup_and_exact_prior_survive_encoding() {
        let mut entries =
            parse("\u{feff}# fixture\nni'hao\t你好\t1\nni'hao\t你好\t99\nnihao\t你好\t10").unwrap();
        classify(&mut entries, "ni'hao=你好", "");
        let words = &entries["nihao"];
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].weight, 99);
        assert!(words[0].everyday);
        assert!(!words[1].everyday);
        encode(&entries);
    }
}

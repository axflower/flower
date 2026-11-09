//! Binary-only CPU inference. Never include this module in the TSF library.
#[path = "assets.rs"]
mod assets;

use flower::ai::{
    eligible_request, spelling_change_span, Request, Response, SPELLING_CANDIDATES,
    SPELLING_MODEL_CALLS, SPELLING_TIMEOUT_MS,
};
use flower::ai::{eligible_text, valid_correction};
use flower::engine::{Lexicon, SpellingIndex};
use ort::{
    session::Session,
    tensor::TensorElementType,
    value::{Tensor, ValueType},
};
use std::{collections::HashMap, ops::Range, path::Path, time::Instant};

const VOCAB_SIZE: usize = 21128;
fn failed(_: impl std::fmt::Display) -> String {
    "AI inference failed".into()
}

pub struct Corrector {
    session: Session,
    vocab: Vec<String>,
    ids: HashMap<char, i64>,
    spelling: SpellingIndex,
}

impl Corrector {
    /// `bundle` is the fixed `ai` directory, not a manifest-selected path.
    pub fn load(bundle: &Path) -> Result<Self, String> {
        let bundle = assets::verify(bundle)?;
        let vocab = std::fs::read_to_string(bundle.join("vocab.txt")).map_err(failed)?;
        let vocab: Vec<String> = vocab.lines().map(str::to_owned).collect();
        if vocab.len() != VOCAB_SIZE
            || vocab[100] != "[UNK]"
            || vocab[101] != "[CLS]"
            || vocab[102] != "[SEP]"
            || vocab[103] != "[MASK]"
        {
            return Err("AI vocabulary invalid".into());
        }
        let ids = vocab
            .iter()
            .enumerate()
            .filter_map(|(i, token)| {
                let mut chars = token.chars();
                let c = chars.next()?;
                (chars.next().is_none() && ('\u{4e00}'..='\u{9fff}').contains(&c))
                    .then_some((c, i as i64))
            })
            .collect();
        assets::restrict_dll_search()?;
        let runtime = bundle.join("onnxruntime.dll");
        let runtime = runtime.to_str().ok_or("AI runtime path invalid")?;
        // Must precede every ORT API call. No environment/PATH fallback.
        ort::init_from(runtime).commit().map_err(failed)?;
        // Fixed hash pins a self-contained graph. In-memory loading supplies no
        // model directory from which arbitrary external tensor files can resolve.
        let model = std::fs::read(bundle.join("model_int8.onnx")).map_err(failed)?;
        let session = Session::builder()
            .map_err(failed)?
            .with_intra_threads(2)
            .map_err(failed)?
            .with_inter_threads(1)
            .map_err(failed)?
            .commit_from_memory(&model)
            .map_err(failed)?;
        if session.inputs.len() != 3 || session.outputs.len() != 1 {
            return Err("AI graph signature invalid".into());
        }
        for name in ["input_ids", "attention_mask", "token_type_ids"] {
            let input = session
                .inputs
                .iter()
                .find(|i| i.name == name)
                .ok_or("AI graph signature invalid")?;
            if !matches!(&input.input_type, ValueType::Tensor { ty: TensorElementType::Int64, dimensions, .. }
                if dimensions.len() == 2 && matches!(dimensions[0], -1 | 1) && dimensions[1] == -1)
            {
                return Err("AI graph signature invalid".into());
            }
        }
        let output = &session.outputs[0];
        if output.name != "logits"
            || !matches!(&output.output_type, ValueType::Tensor { ty: TensorElementType::Float32, dimensions, .. }
                if dimensions.len() == 3 && matches!(dimensions[0], -1 | 1)
                    && dimensions[1] == -1 && dimensions[2] == VOCAB_SIZE as i64)
        {
            return Err("AI graph signature invalid".into());
        }
        Ok(Self {
            session,
            vocab,
            ids,
            spelling: SpellingIndex::new(&Lexicon::builtin().map_err(failed)?),
        })
    }

    pub fn correct(&mut self, text: &str) -> Result<Option<String>, String> {
        if !eligible_text(text) {
            return Err("AI input ineligible".into());
        }
        let ids = self.encode(text)?;
        let logits = self.raw_logits(&ids)?;
        filter_logits(
            text,
            &ids[1..ids.len() - 1],
            &self.vocab,
            &[1, ids.len() as i64, VOCAB_SIZE as i64],
            &logits,
        )
    }

    fn encode(&self, text: &str) -> Result<Vec<i64>, String> {
        let mut ids = vec![101];
        for c in text.chars() {
            ids.push(*self.ids.get(&c).ok_or("AI character unsupported")?);
        }
        ids.push(102);
        Ok(ids)
    }

    fn raw_logits(&mut self, ids: &[i64]) -> Result<Vec<f32>, String> {
        let len = ids.len();
        if ids.iter().any(|&id| !(0..VOCAB_SIZE as i64).contains(&id)) {
            return Err("AI input invalid".into());
        }
        let input =
            Tensor::from_array(([1usize, len], ids.to_vec().into_boxed_slice())).map_err(failed)?;
        let mask = Tensor::from_array(([1usize, len], vec![1i64; len].into_boxed_slice()))
            .map_err(failed)?;
        let types = Tensor::from_array(([1usize, len], vec![0i64; len].into_boxed_slice()))
            .map_err(failed)?;
        let outputs = self
            .session
            .run(
                ort::inputs![
                    "input_ids" => input, "attention_mask" => mask, "token_type_ids" => types
                ]
                .map_err(failed)?,
            )
            .map_err(failed)?;
        let (shape, logits) = outputs["logits"]
            .try_extract_raw_tensor::<f32>()
            .map_err(failed)?;
        if shape != [1, len as i64, VOCAB_SIZE as i64]
            || logits.len() != len * VOCAB_SIZE
            || logits.iter().any(|v| !v.is_finite())
        {
            return Err("AI output invalid".into());
        }
        Ok(logits.to_vec())
    }

    pub fn correct_request(&mut self, request: &Request) -> Result<Response, String> {
        if !eligible_request(request) {
            return Err("AI input ineligible".into());
        }
        let start = Instant::now();
        let mut response = Response {
            id: request.id,
            corrected: None,
            reading: String::new(),
        };
        // Reuse this unmasked forward for both the unchanged CSC and spelling gates.
        let source = self.encode(&request.text)?;
        let logits = self.raw_logits(&source)?;
        let mut budget = Budget { start, calls: 1 };
        response.corrected = if eligible_text(&request.text) {
            filter_logits(
                &request.text,
                &source[1..source.len() - 1],
                &self.vocab,
                &[1, source.len() as i64, VOCAB_SIZE as i64],
                &logits,
            )?
        } else {
            None
        };
        if response.corrected.is_some() || request.raw.is_empty() {
            return Ok(response);
        }
        let candidates: Vec<_> = self
            .spelling
            .candidates_with_source(&request.raw, 8, &request.text, &request.reading)
            .into_iter()
            .filter_map(|c| {
                spelling_change_span(
                    &request.raw,
                    &request.text,
                    &request.reading,
                    &c.text,
                    &c.reading,
                )
                .map(|span| (c, span))
            })
            .collect();
        let mut local_order: Vec<_> = (0..candidates.len()).collect();
        local_order.sort_by_key(|&i| {
            let (_, (old, new)) = &candidates[i];
            (old.len().max(new.len()), old.len() + new.len())
        });
        let mut order = Vec::new();
        for i in (0..candidates.len())
            .zip(local_order)
            .flat_map(|(lexical, local)| [lexical, local])
        {
            if !order.contains(&i) {
                order.push(i);
            }
            if order.len() == SPELLING_CANDIDATES {
                break;
            }
        }
        let candidates: Vec<_> = order.into_iter().map(|i| candidates[i].clone()).collect();
        let mut best: Option<(f64, f64)> = None;
        for (candidate, (old, new)) in candidates.into_iter().take(SPELLING_CANDIDATES) {
            if !budget.available() {
                break;
            }
            let uncertainty = if old.is_empty() {
                let p = old.start.min(source.len() - 3);
                log_probability(row(&logits, p), source[p + 1] as usize).exp()
            } else {
                old.clone()
                    .map(|p| log_probability(row(&logits, p), source[p + 1] as usize).exp())
                    .sum::<f64>()
                    / old.len() as f64
            };
            #[cfg(test)]
            eprintln!(
                "candidate={} source_p={uncertainty:.6} calls={} ms={}",
                candidate.text,
                budget.calls,
                budget.start.elapsed().as_millis()
            );
            if uncertainty >= 0.90 {
                continue;
            }
            let target = self.encode(&candidate.text)?;
            let old_window = window(old.clone(), source.len() - 2);
            let new_window = window(new.clone(), target.len() - 2);
            let mut changed_probabilities = Vec::new();
            let mut target_score = 0.0;
            let mut accepted = true;
            for p in new_window.clone() {
                let Some(masked) = self.masked(&target, p, &mut budget)? else {
                    accepted = false;
                    break;
                };
                let r = row(&masked, p);
                let lp = log_probability(r, target[p + 1] as usize);
                target_score += lp;
                let changed = if new.is_empty() {
                    p == new.start.min(target.len() - 3)
                } else {
                    new.contains(&p) && (old.len() != new.len() || source[p + 1] != target[p + 1])
                };
                if changed {
                    #[cfg(test)]
                    eprintln!(
                        "mask position={p} p={:.6} replacement_margin={:?}",
                        lp.exp(),
                        (old.len() == new.len())
                            .then(|| r[target[p + 1] as usize] - r[source[p + 1] as usize])
                    );
                    let probability = lp.exp();
                    changed_probabilities.push(probability);
                    if !probability.is_finite()
                        || !(0.90..=1.0).contains(&probability)
                        || (old.len() == new.len()
                            && r[target[p + 1] as usize] - r[source[p + 1] as usize] < 4.0)
                    {
                        accepted = false;
                        break;
                    }
                }
            }
            if !accepted {
                continue;
            }
            let mut source_score = 0.0;
            for p in old_window.clone() {
                let Some(masked) = self.masked(&source, p, &mut budget)? else {
                    accepted = false;
                    break;
                };
                source_score += log_probability(row(&masked, p), source[p + 1] as usize);
            }
            if !accepted {
                break;
            }
            let gain =
                target_score / new_window.len() as f64 - source_score / old_window.len() as f64;
            let Some(confidence) = spelling_evidence_confidence(&changed_probabilities, gain)
            else {
                continue;
            };
            #[cfg(test)]
            eprintln!(
                "gain={gain:.6} confidence={confidence:.6} calls={} ms={}",
                budget.calls,
                budget.start.elapsed().as_millis()
            );
            if best.map_or(true, |b| gain > b.0 || (gain == b.0 && confidence > b.1)) {
                best = Some((gain, confidence));
                response.corrected = Some(candidate.text);
                response.reading = candidate.reading;
            }
        }
        Ok(response)
    }

    fn masked(
        &mut self,
        ids: &[i64],
        position: usize,
        budget: &mut Budget,
    ) -> Result<Option<Vec<f32>>, String> {
        if !budget.available() {
            return Ok(None);
        }
        let mut ids = ids.to_vec();
        ids[position + 1] = 103;
        budget.calls += 1;
        let logits = self.raw_logits(&ids)?;
        // A forward itself is not preemptible; never accept evidence after deadline.
        Ok((budget.start.elapsed().as_millis() < SPELLING_TIMEOUT_MS as u128).then_some(logits))
    }
}

/// Called only after complete target/source local masked evidence is available.
/// A weaker changed character requires stronger contextual evidence.
fn spelling_evidence_confidence(probabilities: &[f64], gain: f64) -> Option<f64> {
    if probabilities.is_empty()
        || !gain.is_finite()
        || probabilities
            .iter()
            .any(|p| !p.is_finite() || !(0.90..=1.0).contains(p))
    {
        return None;
    }
    let confidence = probabilities.iter().copied().fold(1.0f64, f64::min);
    let required_gain = if confidence >= 0.95 { 1.0 } else { 2.0 };
    (gain >= required_gain).then_some(confidence)
}

struct Budget {
    start: Instant,
    calls: usize,
}
impl Budget {
    fn available(&self) -> bool {
        self.calls < SPELLING_MODEL_CALLS
            && self.start.elapsed().as_millis() < SPELLING_TIMEOUT_MS as u128
    }
}
fn row(logits: &[f32], p: usize) -> &[f32] {
    &logits[(p + 1) * VOCAB_SIZE..(p + 2) * VOCAB_SIZE]
}
fn log_probability(row: &[f32], id: usize) -> f64 {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    row[id] as f64
        - max
        - row
            .iter()
            .map(|&v| (v as f64 - max).exp())
            .sum::<f64>()
            .ln()
}
fn window(span: Range<usize>, len: usize) -> Range<usize> {
    let start = span.start.saturating_sub(2);
    start..(span.end + 2).min(len).min(start + 7)
}

/// Also usable by deterministic fixtures; it does not initialize ORT.
pub fn filter_logits(
    text: &str,
    ids: &[i64],
    vocab: &[String],
    shape: &[i64],
    logits: &[f32],
) -> Result<Option<String>, String> {
    let len = text.chars().count();
    if !eligible_text(text)
        || ids.len() != len
        || vocab.len() != VOCAB_SIZE
        || shape != [1, (len + 2) as i64, VOCAB_SIZE as i64]
        || logits.len() != (len + 2) * VOCAB_SIZE
        || logits.iter().any(|v| !v.is_finite())
        || ids.iter().any(|&id| !(0..VOCAB_SIZE as i64).contains(&id))
    {
        return Err("AI output invalid".into());
    }
    let mut corrected = String::new();
    for (position, original) in text.chars().enumerate() {
        let row = &logits[(position + 1) * VOCAB_SIZE..(position + 2) * VOCAB_SIZE];
        let (best, &max) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .ok_or("AI output invalid")?;
        if best == ids[position] as usize {
            corrected.push(original);
            continue;
        }
        let denominator: f64 = row.iter().map(|&v| ((v as f64) - max as f64).exp()).sum();
        if 1.0 / denominator < 0.98 || max - row[ids[position] as usize] < 4.0 {
            corrected.push(original);
            continue;
        }
        let mut chars = vocab[best].chars();
        let c = chars.next().ok_or("AI output invalid")?;
        if chars.next().is_some() || !('\u{4e00}'..='\u{9fff}').contains(&c) {
            corrected.push(original);
            continue;
        }
        corrected.push(c);
    }
    Ok(valid_correction(text, &corrected).then_some(corrected))
}

pub fn probe(bundle: &Path, text: &str) -> Result<Option<String>, String> {
    Corrector::load(bundle)?.correct(text)
}

pub fn run_server() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(failed)?;
    let bundle = exe.parent().ok_or("AI bundle unavailable")?.join("ai");
    let mut corrector = None;
    // serve binds the single-instance pipe before this closure can cold-load.
    flower::ai::ipc::serve_requests(move |request| {
        if corrector.is_none() {
            corrector = Some(Corrector::load(&bundle)?);
        }
        corrector
            .as_mut()
            .ok_or("AI unavailable")?
            .correct_request(request)
    })
}

#[cfg(test)]
mod spelling_unit_tests {
    use super::*;

    #[test]
    fn spelling_evidence_numeric_boundaries() {
        for (probability, required_gain) in [
            (0.909324, 2.0),
            (0.90, 2.0),
            (0.95 - 1e-9, 2.0),
            (0.95, 1.0),
            (1.0, 1.0),
        ] {
            assert_eq!(
                spelling_evidence_confidence(&[probability], required_gain - 1e-9),
                None
            );
            assert_eq!(
                spelling_evidence_confidence(&[probability], required_gain),
                Some(probability)
            );
        }
        assert_eq!(spelling_evidence_confidence(&[0.99, 0.909324], 1.5), None);
        assert_eq!(
            spelling_evidence_confidence(&[0.99, 0.909324], 2.0),
            Some(0.909324)
        );
        assert_eq!(
            spelling_evidence_confidence(&[0.909324, 0.99], 2.0),
            Some(0.909324)
        );
        for invalid in [
            0.90 - 1e-9,
            0.0,
            -1.0,
            1.0 + 1e-9,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert_eq!(spelling_evidence_confidence(&[0.99, invalid], 3.0), None);
            assert_eq!(spelling_evidence_confidence(&[invalid, 0.99], 3.0), None);
        }
        for invalid_gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(spelling_evidence_confidence(&[0.99], invalid_gain), None);
        }
        assert_eq!(spelling_evidence_confidence(&[], 3.0), None);
    }

    #[test]
    fn budget_and_local_window_bounds() {
        let budget = Budget {
            start: Instant::now(),
            calls: SPELLING_MODEL_CALLS,
        };
        assert!(!budget.available());
        let expired = Budget {
            start: Instant::now() - std::time::Duration::from_millis(SPELLING_TIMEOUT_MS),
            calls: 0,
        };
        assert!(!expired.available());
        assert_eq!(window(0..1, 7), 0..3);
        assert_eq!(window(3..6, 10), 1..8);
        assert_eq!(window(7..7, 7), 5..7);
        let logits = [0.0, 0.0];
        assert!((log_probability(&logits, 0).exp() - 0.5).abs() < 1e-9);
    }
}

//! Constrained BIOES Viterbi decoding for the privacy-filter model.
//!
//! The model emits 33 per-token logits (`O` plus B/I/E/S for eight entity
//! types). Taking the argmax per token produces malformed tag sequences —
//! the G0 spike measured argmax and Viterbi disagreeing on 5.2% of tokens
//! and, more importantly, on **62.6% of sentences at span level**. So the
//! path is decoded under the BIOES legality constraints, exactly as the
//! reference implementation does.
//!
//! Transition biases come from `viterbi_calibration.json`'s `default`
//! operating point. Every bias in the published calibration is `0.0`; the
//! arithmetic is wired anyway so a future calibration file actually changes
//! the decode instead of being silently ignored.
//!
//! Pure arithmetic — no ONNX Runtime, no tokenizer — so this module compiles
//! and is tested even in a build without the `ner` feature.

use std::collections::HashMap;

/// BIOES position tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    /// Outside any entity.
    O,
    /// Begin a multi-token entity.
    B,
    /// Inside a multi-token entity.
    I,
    /// End a multi-token entity.
    E,
    /// Single-token entity.
    S,
}

/// One row of the model's label table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelInfo {
    /// Full label as written in `config.json` (`B-private_person`, `O`).
    pub name: String,
    pub tag: Tag,
    /// Entity type without the tag prefix; empty for `O`.
    pub entity: String,
}

/// Parse `config.json`'s `id2label` map into a dense, index-ordered table.
///
/// Errors rather than panics on a malformed map: this file is read from disk
/// at load time and a corrupt install must fail the rule, not the process.
pub fn parse_labels(id2label: &HashMap<usize, String>) -> Result<Vec<LabelInfo>, String> {
    let n = id2label.len();
    if n == 0 {
        return Err("config.json id2label is empty".into());
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let name = id2label
            .get(&i)
            .ok_or_else(|| format!("config.json id2label is not dense: missing id {i}"))?;
        let (tag, entity) = if name == "O" {
            (Tag::O, String::new())
        } else {
            let (prefix, rest) = name
                .split_once('-')
                .ok_or_else(|| format!("label '{name}' is neither 'O' nor 'TAG-entity'"))?;
            let tag = match prefix {
                "B" => Tag::B,
                "I" => Tag::I,
                "E" => Tag::E,
                "S" => Tag::S,
                other => return Err(format!("unknown BIOES tag prefix '{other}' in '{name}'")),
            };
            (tag, rest.to_string())
        };
        out.push(LabelInfo {
            name: name.clone(),
            tag,
            entity,
        });
    }
    Ok(out)
}

/// The six transition biases the calibration file can set.
///
/// Field names mirror the JSON keys one-for-one so a reader can check the
/// mapping without a lookup table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransitionBias {
    pub background_stay: f32,
    pub background_to_start: f32,
    pub end_to_background: f32,
    pub end_to_start: f32,
    pub inside_to_continue: f32,
    pub inside_to_end: f32,
}

impl TransitionBias {
    /// Read the `default` operating point out of `viterbi_calibration.json`.
    ///
    /// A missing file or a missing key yields all-zero biases — the published
    /// calibration's own values — rather than an error, because that is the
    /// documented default operating point and not a degraded state.
    pub fn from_calibration_json(v: &serde_json::Value) -> Self {
        let b = &v["operating_points"]["default"]["biases"];
        let get = |k: &str| b.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        Self {
            background_stay: get("transition_bias_background_stay"),
            background_to_start: get("transition_bias_background_to_start"),
            end_to_background: get("transition_bias_end_to_background"),
            end_to_start: get("transition_bias_end_to_start"),
            inside_to_continue: get("transition_bias_inside_to_continue"),
            inside_to_end: get("transition_bias_inside_to_end"),
        }
    }

    /// Bias applied to a legal `from → to` transition.
    fn for_pair(&self, from: &LabelInfo, to: &LabelInfo) -> f32 {
        match (from.tag, to.tag) {
            (Tag::O, Tag::O) => self.background_stay,
            (Tag::O, Tag::B) | (Tag::O, Tag::S) => self.background_to_start,
            (Tag::E | Tag::S, Tag::O) => self.end_to_background,
            (Tag::E | Tag::S, Tag::B) | (Tag::E | Tag::S, Tag::S) => self.end_to_start,
            (Tag::B | Tag::I, Tag::I) => self.inside_to_continue,
            (Tag::B | Tag::I, Tag::E) => self.inside_to_end,
            _ => 0.0,
        }
    }
}

/// Legality matrix `allowed[from][to]` for BIOES:
///
/// ```text
/// O           → O | B-x | S-x
/// B-x | I-x   → I-x | E-x          (same entity only)
/// E-x | S-x   → O | B-y | S-y
/// ```
pub fn build_transitions(labels: &[LabelInfo]) -> Vec<Vec<bool>> {
    let n = labels.len();
    let mut m = vec![vec![false; n]; n];
    for (i, from) in labels.iter().enumerate() {
        for (j, to) in labels.iter().enumerate() {
            m[i][j] = match from.tag {
                Tag::O | Tag::E | Tag::S => matches!(to.tag, Tag::O | Tag::B | Tag::S),
                Tag::B | Tag::I => {
                    matches!(to.tag, Tag::I | Tag::E) && to.entity == from.entity
                }
            };
        }
    }
    m
}

/// May a sequence start on this label? (Not mid-entity.)
pub fn start_allowed(l: &LabelInfo) -> bool {
    matches!(l.tag, Tag::O | Tag::B | Tag::S)
}

/// May a sequence end on this label? (Not mid-entity.)
pub fn end_allowed(l: &LabelInfo) -> bool {
    matches!(l.tag, Tag::O | Tag::E | Tag::S)
}

/// Numerically stable log-softmax of one logits row.
pub fn log_softmax_row(row: &[f32]) -> Vec<f32> {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !max.is_finite() {
        // All-NaN / all-(-inf) row: emit a flat distribution rather than NaNs
        // that would poison the whole dynamic-programming table.
        return vec![0.0; row.len()];
    }
    let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
    let lse = max + sum.ln();
    row.iter().map(|v| v - lse).collect()
}

const NEG: f32 = -1.0e30;

/// Constrained Viterbi over `emissions[t][label]` (log-probabilities).
///
/// Returns the best legal label path. On a degenerate input (every legal end
/// unreachable) it falls back to the best unconstrained end so the caller
/// still gets a path instead of an error — the span builder is tolerant of
/// malformed paths by construction.
pub fn viterbi(
    emissions: &[Vec<f32>],
    labels: &[LabelInfo],
    transitions: &[Vec<bool>],
    bias: &TransitionBias,
) -> Vec<usize> {
    let t_len = emissions.len();
    let n = labels.len();
    if t_len == 0 || n == 0 {
        return Vec::new();
    }

    let mut dp = vec![vec![NEG; n]; t_len];
    let mut bp = vec![vec![0usize; n]; t_len];

    for k in 0..n {
        if start_allowed(&labels[k]) {
            dp[0][k] = emissions[0][k];
        }
    }

    for t in 1..t_len {
        for k in 0..n {
            let mut best = NEG;
            let mut arg = 0usize;
            for j in 0..n {
                if dp[t - 1][j] <= NEG || !transitions[j][k] {
                    continue;
                }
                let score = dp[t - 1][j] + bias.for_pair(&labels[j], &labels[k]);
                if score > best {
                    best = score;
                    arg = j;
                }
            }
            if best > NEG {
                dp[t][k] = best + emissions[t][k];
                bp[t][k] = arg;
            }
        }
    }

    let last_row = &dp[t_len - 1];
    let mut best = NEG;
    let mut last = 0usize;
    for (k, score) in last_row.iter().enumerate() {
        if end_allowed(&labels[k]) && *score > best {
            best = *score;
            last = k;
        }
    }
    if best <= NEG {
        // Degenerate: no legal end state is reachable. Take the best state of
        // any kind so the caller still gets a path — `path_to_spans` tolerates
        // a malformed one.
        for (k, score) in last_row.iter().enumerate() {
            if *score > best {
                best = *score;
                last = k;
            }
        }
    }

    let mut path = vec![0usize; t_len];
    path[t_len - 1] = last;
    for t in (1..t_len).rev() {
        path[t - 1] = bp[t][path[t]];
    }
    path
}

/// One decoded entity span, in **byte offsets into the text that was scored**.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSpan {
    /// Model entity label (`private_person`, …) — not a DuDuClaw category.
    pub label: String,
    pub start: usize,
    pub end: usize,
}

/// Turn a per-token BIOES path into entity spans using the tokenizer's byte
/// offsets.
///
/// Tolerant of malformed paths (an `I-` with no open span opens one) so the
/// same function serves a constrained and an unconstrained decode. Adjacent
/// same-entity spans are NOT merged — that is the model's own segmentation
/// and merging it would widen masks the model did not ask for.
pub fn path_to_spans(
    path: &[usize],
    labels: &[LabelInfo],
    offsets: &[(usize, usize)],
) -> Vec<RawSpan> {
    let mut spans: Vec<RawSpan> = Vec::new();
    let mut open: Option<(String, usize, usize)> = None;

    fn flush(open: &mut Option<(String, usize, usize)>, spans: &mut Vec<RawSpan>) {
        if let Some((label, start, end)) = open.take() {
            spans.push(RawSpan { label, start, end });
        }
    }

    for (t, &k) in path.iter().enumerate() {
        let Some(li) = labels.get(k) else { continue };
        let Some(&(os, oe)) = offsets.get(t) else { continue };
        match li.tag {
            Tag::O => flush(&mut open, &mut spans),
            Tag::S => {
                flush(&mut open, &mut spans);
                spans.push(RawSpan {
                    label: li.entity.clone(),
                    start: os,
                    end: oe,
                });
            }
            Tag::B => {
                flush(&mut open, &mut spans);
                open = Some((li.entity.clone(), os, oe));
            }
            Tag::I => match open.as_mut() {
                Some((ent, _, end)) if *ent == li.entity => *end = oe,
                _ => {
                    flush(&mut open, &mut spans);
                    open = Some((li.entity.clone(), os, oe));
                }
            },
            Tag::E => {
                match open.as_mut() {
                    Some((ent, _, end)) if *ent == li.entity => *end = oe,
                    _ => {
                        flush(&mut open, &mut spans);
                        open = Some((li.entity.clone(), os, oe));
                    }
                }
                flush(&mut open, &mut spans);
            }
        }
    }
    flush(&mut open, &mut spans);
    spans.retain(|s| s.end > s.start);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 33-label table, abbreviated to three entity types so the
    /// synthetic logits stay readable. Layout matches the model's:
    /// index 0 is `O`, then B/I/E/S per entity.
    fn labels_for(entities: &[&str]) -> Vec<LabelInfo> {
        let mut map = HashMap::new();
        map.insert(0usize, "O".to_string());
        let mut idx = 1usize;
        for e in entities {
            for tag in ["B", "I", "E", "S"] {
                map.insert(idx, format!("{tag}-{e}"));
                idx += 1;
            }
        }
        parse_labels(&map).unwrap()
    }

    /// Emissions that force `path` by giving each wanted label a large score.
    fn emissions_forcing(path: &[usize], n_labels: usize) -> Vec<Vec<f32>> {
        path.iter()
            .map(|&k| {
                let mut row = vec![0.0f32; n_labels];
                row[k] = 10.0;
                row
            })
            .collect()
    }

    #[test]
    fn parse_labels_reads_the_real_table_shape() {
        let labels = labels_for(&["private_person", "private_phone"]);
        assert_eq!(labels.len(), 9);
        assert_eq!(labels[0].tag, Tag::O);
        assert_eq!(labels[0].entity, "");
        assert_eq!(labels[1].name, "B-private_person");
        assert_eq!(labels[1].tag, Tag::B);
        assert_eq!(labels[1].entity, "private_person");
        assert_eq!(labels[8].tag, Tag::S);
        assert_eq!(labels[8].entity, "private_phone");
    }

    #[test]
    fn parse_labels_rejects_a_sparse_or_malformed_map() {
        let mut sparse = HashMap::new();
        sparse.insert(0usize, "O".to_string());
        sparse.insert(2usize, "S-x".to_string());
        assert!(parse_labels(&sparse).unwrap_err().contains("not dense"));

        let mut bad_tag = HashMap::new();
        bad_tag.insert(0usize, "O".to_string());
        bad_tag.insert(1usize, "Q-x".to_string());
        assert!(parse_labels(&bad_tag).unwrap_err().contains("Q"));

        let mut no_dash = HashMap::new();
        no_dash.insert(0usize, "O".to_string());
        no_dash.insert(1usize, "Bperson".to_string());
        assert!(parse_labels(&no_dash).unwrap_err().contains("TAG-entity"));

        assert!(parse_labels(&HashMap::new()).unwrap_err().contains("empty"));
    }

    #[test]
    fn transition_matrix_encodes_bioes_legality() {
        let l = labels_for(&["a", "b"]);
        let m = build_transitions(&l);
        // indices: 0=O, 1..4 = B/I/E/S-a, 5..8 = B/I/E/S-b
        assert!(m[0][0], "O → O");
        assert!(m[0][1], "O → B-a");
        assert!(m[0][4], "O → S-a");
        assert!(!m[0][2], "O → I-a must be illegal");
        assert!(!m[0][3], "O → E-a must be illegal");

        assert!(m[1][2], "B-a → I-a");
        assert!(m[1][3], "B-a → E-a");
        assert!(!m[1][0], "B-a → O must be illegal (entity left open)");
        assert!(!m[1][6], "B-a → I-b must be illegal (entity switch)");
        assert!(!m[1][1], "B-a → B-a must be illegal");

        assert!(m[2][2] && m[2][3], "I-a → I-a | E-a");
        assert!(!m[2][0], "I-a → O must be illegal");

        assert!(m[3][0] && m[3][1] && m[3][4] && m[3][5]);
        assert!(!m[3][2], "E-a → I-a must be illegal");
        assert!(m[4][0] && m[4][5], "S-a → O | B-b");
        assert!(!m[4][2], "S-a → I-a must be illegal");
    }

    #[test]
    fn start_and_end_constraints_exclude_mid_entity_tags() {
        let l = labels_for(&["a"]);
        assert!(start_allowed(&l[0]) && start_allowed(&l[1]) && start_allowed(&l[4]));
        assert!(!start_allowed(&l[2]) && !start_allowed(&l[3]));
        assert!(end_allowed(&l[0]) && end_allowed(&l[3]) && end_allowed(&l[4]));
        assert!(!end_allowed(&l[1]) && !end_allowed(&l[2]));
    }

    #[test]
    fn viterbi_refuses_an_illegal_argmax_and_returns_a_legal_path() {
        let l = labels_for(&["a"]);
        let n = l.len();
        // Argmax would be [I-a, I-a] — illegal as a start AND as an end.
        let emis = emissions_forcing(&[2, 2], n);
        let path = viterbi(&emis, &l, &build_transitions(&l), &TransitionBias::default());
        assert!(start_allowed(&l[path[0]]), "path starts on {:?}", l[path[0]]);
        assert!(end_allowed(&l[path[1]]), "path ends on {:?}", l[path[1]]);
        // And legal step-by-step.
        let m = build_transitions(&l);
        assert!(m[path[0]][path[1]]);
    }

    #[test]
    fn viterbi_keeps_a_legal_argmax_intact() {
        let l = labels_for(&["a"]);
        let want = [0usize, 1, 2, 3, 0]; // O B-a I-a E-a O
        let emis = emissions_forcing(&want, l.len());
        let path = viterbi(&emis, &l, &build_transitions(&l), &TransitionBias::default());
        assert_eq!(path, want.to_vec());
    }

    #[test]
    fn viterbi_on_empty_input_is_empty() {
        let l = labels_for(&["a"]);
        assert!(viterbi(&[], &l, &build_transitions(&l), &TransitionBias::default()).is_empty());
    }

    #[test]
    fn calibration_bias_actually_changes_the_decode() {
        let l = labels_for(&["a"]);
        let n = l.len();
        // Two near-ties on the second token: S-a (idx 4) vs O (idx 0).
        let mut emis = vec![vec![0.0f32; n]; 2];
        emis[0][0] = 10.0; // token 0 clearly O
        emis[1][0] = 1.0; // token 1: O …
        emis[1][4] = 1.0; // … or S-a, dead even
        let trans = build_transitions(&l);

        let stay = TransitionBias {
            background_stay: 5.0,
            ..TransitionBias::default()
        };
        assert_eq!(viterbi(&emis, &l, &trans, &stay)[1], 0, "stay bias must win O");

        let start = TransitionBias {
            background_to_start: 5.0,
            ..TransitionBias::default()
        };
        assert_eq!(viterbi(&emis, &l, &trans, &start)[1], 4, "start bias must win S-a");
    }

    #[test]
    fn published_calibration_is_all_zero_biases() {
        // Transcribed from the pinned `viterbi_calibration.json`. If a future
        // revision ships non-zero biases this test is the tripwire.
        let v: serde_json::Value = serde_json::from_str(
            r#"{"operating_points":{"default":{"biases":{
                "transition_bias_background_stay":0.0,
                "transition_bias_background_to_start":0.0,
                "transition_bias_end_to_background":0.0,
                "transition_bias_end_to_start":0.0,
                "transition_bias_inside_to_continue":0.0,
                "transition_bias_inside_to_end":0.0}}}}"#,
        )
        .unwrap();
        assert_eq!(
            TransitionBias::from_calibration_json(&v),
            TransitionBias::default()
        );
    }

    #[test]
    fn calibration_parse_degrades_to_zero_on_a_missing_file_shape() {
        let v = serde_json::json!({});
        assert_eq!(
            TransitionBias::from_calibration_json(&v),
            TransitionBias::default()
        );
    }

    #[test]
    fn log_softmax_normalises_and_survives_a_degenerate_row() {
        let out = log_softmax_row(&[1.0, 2.0, 3.0]);
        let sum: f32 = out.iter().map(|v| v.exp()).sum();
        assert!((sum - 1.0).abs() < 1e-5, "sum was {sum}");
        assert!(out[2] > out[1] && out[1] > out[0]);

        let degenerate = log_softmax_row(&[f32::NEG_INFINITY, f32::NEG_INFINITY]);
        assert!(degenerate.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn single_token_entity_becomes_one_span() {
        let l = labels_for(&["private_phone"]);
        let offsets = [(0, 3), (3, 15), (15, 16)];
        let spans = path_to_spans(&[0, 4, 0], &l, &offsets);
        assert_eq!(
            spans,
            vec![RawSpan { label: "private_phone".into(), start: 3, end: 15 }]
        );
    }

    #[test]
    fn b_i_e_sequence_becomes_one_span_covering_all_three_tokens() {
        let l = labels_for(&["private_person"]);
        let offsets = [(0, 2), (2, 5), (5, 11), (11, 14), (14, 20)];
        let spans = path_to_spans(&[0, 1, 2, 3, 0], &l, &offsets);
        assert_eq!(
            spans,
            vec![RawSpan { label: "private_person".into(), start: 2, end: 14 }]
        );
    }

    #[test]
    fn adjacent_same_entity_spans_are_not_merged() {
        let l = labels_for(&["private_person"]);
        let offsets = [(0, 3), (3, 6)];
        let spans = path_to_spans(&[4, 4], &l, &offsets);
        assert_eq!(spans.len(), 2, "model segmentation must be preserved: {spans:?}");
        assert_eq!(spans[0].end, 3);
        assert_eq!(spans[1].start, 3);
    }

    #[test]
    fn unterminated_entity_at_the_end_still_yields_a_span() {
        let l = labels_for(&["secret"]);
        let offsets = [(0, 1), (1, 4), (4, 9)];
        let spans = path_to_spans(&[0, 1, 2], &l, &offsets);
        assert_eq!(spans, vec![RawSpan { label: "secret".into(), start: 1, end: 9 }]);
    }

    #[test]
    fn entity_switch_mid_span_closes_the_previous_one() {
        // Only reachable from an unconstrained path, but the builder must not
        // silently glue two entity types into one span.
        let l = labels_for(&["a", "b"]);
        let offsets = [(0, 2), (2, 4)];
        let spans = path_to_spans(&[1, 6], &l, &offsets); // B-a then I-b
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].label, "a");
        assert_eq!(spans[1].label, "b");
    }

    #[test]
    fn zero_width_offsets_are_dropped() {
        let l = labels_for(&["a"]);
        let offsets = [(5, 5)];
        assert!(path_to_spans(&[4], &l, &offsets).is_empty());
    }

    #[test]
    fn path_longer_than_offsets_is_ignored_not_panicked() {
        let l = labels_for(&["a"]);
        let offsets = [(0, 2)];
        let spans = path_to_spans(&[4, 4, 4], &l, &offsets);
        assert_eq!(spans.len(), 1);
    }
}

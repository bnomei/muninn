use std::collections::HashMap;

use crate::envelope::MuninnEnvelopeV1;
use crate::runner::PipelineOutcome;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct UncertainSpan {
    start: usize,
    end: usize,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct ReplacementCandidate {
    from: String,
    to: String,
    score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AcceptedReplacement {
    start: usize,
    end: usize,
    from: String,
    to: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    pub min_top_score: f32,
    pub min_margin: f32,
    pub acronym_min_top_score: f32,
    pub acronym_min_margin: f32,
    pub short_span_max_chars: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_top_score: 0.84,
            min_margin: 0.10,
            acronym_min_top_score: 0.90,
            acronym_min_margin: 0.15,
            short_span_max_chars: 3,
        }
    }
}

impl From<crate::config::ScoringConfig> for Thresholds {
    fn from(value: crate::config::ScoringConfig) -> Self {
        Self {
            min_top_score: value.min_top_score,
            min_margin: value.min_margin,
            acronym_min_top_score: value.acronym_min_top_score,
            acronym_min_margin: value.acronym_min_margin,
            ..Self::default()
        }
    }
}

impl From<&crate::config::ScoringConfig> for Thresholds {
    fn from(value: &crate::config::ScoringConfig) -> Self {
        Self {
            min_top_score: value.min_top_score,
            min_margin: value.min_margin,
            acronym_min_top_score: value.acronym_min_top_score,
            acronym_min_margin: value.acronym_min_margin,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanMetadata {
    pub is_acronym: bool,
    pub char_len: usize,
}

impl SpanMetadata {
    #[must_use]
    pub const fn new(char_len: usize, is_acronym: bool) -> Self {
        Self {
            is_acronym,
            char_len,
        }
    }

    #[must_use]
    pub const fn is_acronym_or_short(self, short_span_max_chars: usize) -> bool {
        self.is_acronym || self.char_len <= short_span_max_chars
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplacementDecisionInput {
    pub candidate_scores: Vec<f32>,
    pub span: SpanMetadata,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    BelowTopThreshold,
    BelowMarginThreshold,
    Accepted,
    NoCandidates,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ReplacementDecision {
    pub accepted: bool,
    pub reason: DecisionReason,
    pub top_score: Option<f32>,
    pub second_score: Option<f32>,
    pub margin: Option<f32>,
    pub used_strict_thresholds: bool,
}

#[must_use]
pub fn decide_replacement(
    input: &ReplacementDecisionInput,
    thresholds: &Thresholds,
) -> ReplacementDecision {
    let (top_score, second_score) = top_two_scores(&input.candidate_scores);

    let Some(top_score) = top_score else {
        return ReplacementDecision {
            accepted: false,
            reason: DecisionReason::NoCandidates,
            top_score: None,
            second_score: None,
            margin: None,
            used_strict_thresholds: false,
        };
    };

    let used_strict_thresholds = input
        .span
        .is_acronym_or_short(thresholds.short_span_max_chars);
    let (min_top_score, min_margin) = if used_strict_thresholds {
        (
            thresholds.acronym_min_top_score,
            thresholds.acronym_min_margin,
        )
    } else {
        (thresholds.min_top_score, thresholds.min_margin)
    };

    let margin = top_score - second_score.unwrap_or(0.0);

    if top_score < min_top_score {
        return ReplacementDecision {
            accepted: false,
            reason: DecisionReason::BelowTopThreshold,
            top_score: Some(top_score),
            second_score,
            margin: Some(margin),
            used_strict_thresholds,
        };
    }

    if margin < min_margin {
        return ReplacementDecision {
            accepted: false,
            reason: DecisionReason::BelowMarginThreshold,
            top_score: Some(top_score),
            second_score,
            margin: Some(margin),
            used_strict_thresholds,
        };
    }

    ReplacementDecision {
        accepted: true,
        reason: DecisionReason::Accepted,
        top_score: Some(top_score),
        second_score,
        margin: Some(margin),
        used_strict_thresholds,
    }
}

fn top_two_scores(scores: &[f32]) -> (Option<f32>, Option<f32>) {
    let mut top: Option<f32> = None;
    let mut second: Option<f32> = None;

    for score in scores.iter().copied().filter(|score| score.is_finite()) {
        match top {
            None => top = Some(score),
            Some(current_top) if score > current_top => {
                second = top;
                top = Some(score);
            }
            Some(_) => {
                if second.map_or(true, |current_second| score > current_second) {
                    second = Some(score);
                }
            }
        }
    }

    (top, second)
}

pub fn apply_scored_replacements_to_outcome<T>(outcome: &mut PipelineOutcome, thresholds: T) -> bool
where
    T: Into<Thresholds>,
{
    let thresholds = thresholds.into();
    match outcome {
        PipelineOutcome::Completed { envelope, .. }
        | PipelineOutcome::FallbackRaw { envelope, .. } => {
            apply_scored_replacements_to_envelope(envelope, &thresholds)
        }
        PipelineOutcome::Aborted { .. } => false,
    }
}

pub fn apply_scored_replacements_to_envelope(
    envelope: &mut MuninnEnvelopeV1,
    thresholds: &Thresholds,
) -> bool {
    if non_empty_text(&envelope.output.final_text).is_some() {
        return false;
    }

    let Some(raw_text) = non_empty_text(&envelope.transcript.raw_text) else {
        return false;
    };

    let Some(mut replacements) = scored_replacement_plan(
        raw_text,
        &envelope.uncertain_spans,
        &envelope.replacements,
        thresholds,
    ) else {
        return false;
    };
    if replacements.is_empty() {
        return false;
    }

    replacements.sort_by_key(|replacement| (replacement.start, replacement.end));
    replacements.dedup_by(|left, right| {
        left.start == right.start
            && left.end == right.end
            && left.from == right.from
            && left.to == right.to
    });

    let Some(final_text) = render_replacements(raw_text, &replacements) else {
        return false;
    };
    if final_text == raw_text {
        return false;
    }

    envelope.output.final_text = Some(final_text);
    true
}

fn scored_replacement_plan(
    raw_text: &str,
    uncertain_spans: &[serde_json::Value],
    replacements: &[serde_json::Value],
    thresholds: &Thresholds,
) -> Option<Vec<AcceptedReplacement>> {
    let parsed_spans: Vec<_> = uncertain_spans
        .iter()
        .filter_map(|value| serde_json::from_value::<UncertainSpan>(value.clone()).ok())
        .collect();
    let parsed_replacements: Vec<_> = replacements
        .iter()
        .filter_map(|value| serde_json::from_value::<ReplacementCandidate>(value.clone()).ok())
        .collect();
    if parsed_spans.is_empty() || parsed_replacements.is_empty() {
        return Some(Vec::new());
    }

    let mut replacements_by_source = HashMap::<&str, Vec<&ReplacementCandidate>>::new();
    for replacement in &parsed_replacements {
        replacements_by_source
            .entry(replacement.from.as_str())
            .or_default()
            .push(replacement);
    }

    let mut accepted = Vec::new();
    for span in parsed_spans {
        if !span_offsets_match(raw_text, &span) {
            return None;
        }

        let Some(candidates) = replacements_by_source.get(span.text.as_str()) else {
            continue;
        };

        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: candidates.iter().map(|candidate| candidate.score).collect(),
                span: SpanMetadata::new(
                    span.text.chars().count(),
                    looks_like_acronym(&span.text)
                        || candidates
                            .iter()
                            .any(|candidate| looks_like_acronym(candidate.to.as_str())),
                ),
            },
            thresholds,
        );
        if !decision.accepted {
            continue;
        }

        let Some(best_candidate) = candidates.iter().copied().max_by(|left, right| {
            left.score
                .partial_cmp(&right.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            continue;
        };

        accepted.push(AcceptedReplacement {
            start: span.start,
            end: span.end,
            from: span.text,
            to: best_candidate.to.clone(),
        });
    }

    Some(accepted)
}

fn render_replacements(raw_text: &str, replacements: &[AcceptedReplacement]) -> Option<String> {
    let mut rendered = String::with_capacity(raw_text.len());
    let mut cursor = 0_usize;

    for replacement in replacements {
        if replacement.start > replacement.end
            || replacement.end > raw_text.len()
            || !raw_text.is_char_boundary(replacement.start)
            || !raw_text.is_char_boundary(replacement.end)
            || replacement.start < cursor
        {
            return None;
        }

        let source = raw_text.get(replacement.start..replacement.end)?;
        if source != replacement.from {
            return None;
        }

        rendered.push_str(&raw_text[cursor..replacement.start]);
        rendered.push_str(&replacement.to);
        cursor = replacement.end;
    }

    rendered.push_str(&raw_text[cursor..]);
    Some(rendered)
}

fn span_offsets_match(raw_text: &str, span: &UncertainSpan) -> bool {
    span.start <= span.end
        && raw_text
            .get(span.start..span.end)
            .is_some_and(|slice| slice == span.text)
}

fn looks_like_acronym(text: &str) -> bool {
    let normalized: String = text
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    normalized.len() > 1
        && normalized.chars().any(|ch| ch.is_ascii_alphabetic())
        && normalized
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn non_empty_text(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_scored_replacements_to_envelope, apply_scored_replacements_to_outcome,
        decide_replacement, DecisionReason, ReplacementDecisionInput, SpanMetadata, Thresholds,
    };
    use crate::envelope::MuninnEnvelopeV1;
    use crate::runner::PipelineOutcome;
    use serde_json::json;

    fn default_thresholds() -> Thresholds {
        Thresholds::default()
    }

    #[test]
    fn accepts_when_top_and_margin_meet_thresholds() {
        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: vec![0.91, 0.70, 0.65],
                span: SpanMetadata::new(10, false),
            },
            &default_thresholds(),
        );

        assert!(decision.accepted);
        assert_eq!(decision.reason, DecisionReason::Accepted);
        assert_eq!(decision.top_score, Some(0.91));
        assert_eq!(decision.second_score, Some(0.70));
        assert!(decision
            .margin
            .is_some_and(|margin| (margin - 0.21).abs() < 1.0e-6));
        assert!(!decision.used_strict_thresholds);
    }

    #[test]
    fn skips_when_top_score_is_below_threshold() {
        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: vec![0.83, 0.60],
                span: SpanMetadata::new(9, false),
            },
            &default_thresholds(),
        );

        assert!(!decision.accepted);
        assert_eq!(decision.reason, DecisionReason::BelowTopThreshold);
        assert_eq!(decision.top_score, Some(0.83));
    }

    #[test]
    fn skips_when_margin_is_below_threshold() {
        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: vec![0.91, 0.86],
                span: SpanMetadata::new(8, false),
            },
            &default_thresholds(),
        );

        assert!(!decision.accepted);
        assert_eq!(decision.reason, DecisionReason::BelowMarginThreshold);
        assert_eq!(decision.top_score, Some(0.91));
        assert_eq!(decision.second_score, Some(0.86));
        assert!(decision
            .margin
            .is_some_and(|margin| (margin - 0.05).abs() < 1.0e-6));
    }

    #[test]
    fn acronym_spans_use_stricter_thresholds() {
        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: vec![0.88, 0.76],
                span: SpanMetadata::new(2, true),
            },
            &default_thresholds(),
        );

        assert!(!decision.accepted);
        assert_eq!(decision.reason, DecisionReason::BelowTopThreshold);
        assert_eq!(decision.top_score, Some(0.88));
        assert_eq!(decision.second_score, Some(0.76));
        assert!(decision.used_strict_thresholds);
    }

    #[test]
    fn skips_when_no_candidates_exist() {
        let decision = decide_replacement(
            &ReplacementDecisionInput {
                candidate_scores: vec![],
                span: SpanMetadata::new(5, false),
            },
            &default_thresholds(),
        );

        assert!(!decision.accepted);
        assert_eq!(decision.reason, DecisionReason::NoCandidates);
        assert_eq!(decision.top_score, None);
        assert_eq!(decision.second_score, None);
        assert_eq!(decision.margin, None);
    }

    #[test]
    fn materializes_output_final_text_for_accepted_replacements() {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:00:00Z")
            .with_transcript_raw_text("ship to sf")
            .push_uncertain_span(json!({"start": 8, "end": 10, "text": "sf"}))
            .push_replacement(json!({"from": "sf", "to": "San Francisco", "score": 0.93}));

        let applied = apply_scored_replacements_to_envelope(&mut envelope, &default_thresholds());

        assert!(applied);
        assert_eq!(
            envelope.output.final_text.as_deref(),
            Some("ship to San Francisco")
        );
    }

    #[test]
    fn leaves_output_empty_when_replacement_scores_are_ambiguous() {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:00:00Z")
            .with_transcript_raw_text("ship to sf")
            .push_uncertain_span(json!({"start": 8, "end": 10, "text": "sf"}))
            .push_replacement(json!({"from": "sf", "to": "San Francisco", "score": 0.91}))
            .push_replacement(json!({"from": "sf", "to": "South Ferry", "score": 0.86}));

        let applied = apply_scored_replacements_to_envelope(&mut envelope, &default_thresholds());

        assert!(!applied);
        assert!(envelope.output.final_text.is_none());
    }

    #[test]
    fn leaves_envelope_unchanged_when_span_offsets_are_invalid() {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:00:00Z")
            .with_transcript_raw_text("ship to sf")
            .push_uncertain_span(json!({"start": 0, "end": 2, "text": "sf"}))
            .push_replacement(json!({"from": "sf", "to": "San Francisco", "score": 0.93}));

        let applied = apply_scored_replacements_to_envelope(&mut envelope, &default_thresholds());

        assert!(!applied);
        assert!(envelope.output.final_text.is_none());
    }

    #[test]
    fn outcome_helper_updates_completed_outcomes() {
        let mut outcome = PipelineOutcome::Completed {
            envelope: MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:00:00Z")
                .with_transcript_raw_text("ship to sf")
                .push_uncertain_span(json!({"start": 8, "end": 10, "text": "sf"}))
                .push_replacement(json!({"from": "sf", "to": "San Francisco", "score": 0.93})),
            trace: Vec::new(),
        };

        let applied = apply_scored_replacements_to_outcome(&mut outcome, default_thresholds());

        assert!(applied);
        match outcome {
            PipelineOutcome::Completed { envelope, .. } => {
                assert_eq!(
                    envelope.output.final_text.as_deref(),
                    Some("ship to San Francisco")
                );
            }
            other => panic!("expected completed outcome, got {other:?}"),
        }
    }
}

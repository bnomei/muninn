use serde::{Deserialize, Serialize};

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
                if second.is_none_or(|current_second| score > current_second) {
                    second = Some(score);
                }
            }
        }
    }

    (top, second)
}

#[cfg(test)]
mod tests {
    use super::{
        decide_replacement, DecisionReason, ReplacementDecisionInput, SpanMetadata, Thresholds,
    };

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
}

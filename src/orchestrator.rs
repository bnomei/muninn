use crate::envelope::MuninnEnvelopeV1;
use crate::runner::{PipelineOutcome, PipelineStopReason};
use serde::Serialize;

#[derive(Debug, Clone, Default)]
pub struct Orchestrator;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InjectionRoute {
    pub target: InjectionTarget,
    pub reason: InjectionRouteReason,
    pub pipeline_stop_reason: Option<PipelineStopReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum InjectionTarget {
    OutputFinalText(String),
    TranscriptRawText(String),
    None,
}

impl InjectionTarget {
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::OutputFinalText(text) | Self::TranscriptRawText(text) => Some(text.as_str()),
            Self::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InjectionRouteReason {
    SelectedOutputFinalText,
    SelectedTranscriptRawText,
    NoInjectableText,
    PipelineAborted,
}

impl Orchestrator {
    #[must_use]
    pub fn route_injection(outcome: &PipelineOutcome) -> InjectionRoute {
        match outcome {
            PipelineOutcome::Completed { envelope, .. } => route_envelope(envelope, None),
            PipelineOutcome::FallbackRaw {
                envelope, reason, ..
            } => route_envelope(envelope, Some(reason.clone())),
            PipelineOutcome::Aborted { reason, .. } => InjectionRoute {
                target: InjectionTarget::None,
                reason: InjectionRouteReason::PipelineAborted,
                pipeline_stop_reason: Some(reason.clone()),
            },
        }
    }
}

fn route_envelope(
    envelope: &MuninnEnvelopeV1,
    pipeline_stop_reason: Option<PipelineStopReason>,
) -> InjectionRoute {
    if let Some(final_text) = non_empty_text(&envelope.output.final_text) {
        return InjectionRoute {
            target: InjectionTarget::OutputFinalText(final_text.to_owned()),
            reason: InjectionRouteReason::SelectedOutputFinalText,
            pipeline_stop_reason,
        };
    }

    if let Some(raw_text) = non_empty_text(&envelope.transcript.raw_text) {
        return InjectionRoute {
            target: InjectionTarget::TranscriptRawText(raw_text.to_owned()),
            reason: InjectionRouteReason::SelectedTranscriptRawText,
            pipeline_stop_reason,
        };
    }

    InjectionRoute {
        target: InjectionTarget::None,
        reason: InjectionRouteReason::NoInjectableText,
        pipeline_stop_reason,
    }
}

fn non_empty_text(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{InjectionRouteReason, InjectionTarget, Orchestrator};
    use crate::envelope::MuninnEnvelopeV1;
    use crate::runner::{PipelineOutcome, PipelineStopReason, StepFailureKind};

    #[test]
    fn completed_prefers_output_final_text_over_transcript_raw_text() {
        let outcome = PipelineOutcome::Completed {
            envelope: sample_envelope()
                .with_output_final_text("Ship to San Francisco")
                .with_transcript_raw_text("ship to sf"),
            trace: Vec::new(),
        };

        let route = Orchestrator::route_injection(&outcome);

        assert_eq!(
            route.target,
            InjectionTarget::OutputFinalText("Ship to San Francisco".to_string())
        );
        assert_eq!(route.target.text(), Some("Ship to San Francisco"));
        assert_eq!(route.reason, InjectionRouteReason::SelectedOutputFinalText);
        assert_eq!(route.pipeline_stop_reason, None);
    }

    #[test]
    fn fallback_uses_non_empty_transcript_raw_text_and_preserves_reason() {
        let fallback_reason = sample_step_failed_reason();
        let outcome = PipelineOutcome::FallbackRaw {
            envelope: sample_envelope()
                .with_output_final_text("")
                .with_transcript_raw_text("ship to sf"),
            trace: Vec::new(),
            reason: fallback_reason.clone(),
        };

        let route = Orchestrator::route_injection(&outcome);

        assert_eq!(
            route.target,
            InjectionTarget::TranscriptRawText("ship to sf".to_string())
        );
        assert_eq!(
            route.reason,
            InjectionRouteReason::SelectedTranscriptRawText
        );
        assert_eq!(route.pipeline_stop_reason, Some(fallback_reason));
    }

    #[test]
    fn aborted_never_injects_and_preserves_reason_for_diagnostics() {
        let abort_reason = PipelineStopReason::GlobalDeadlineExceeded {
            deadline_ms: 1_500,
            step_id: Some("postprocess".to_string()),
        };
        let outcome = PipelineOutcome::Aborted {
            trace: Vec::new(),
            reason: abort_reason.clone(),
        };

        let route = Orchestrator::route_injection(&outcome);

        assert_eq!(route.target, InjectionTarget::None);
        assert_eq!(route.target.text(), None);
        assert_eq!(route.reason, InjectionRouteReason::PipelineAborted);
        assert_eq!(route.pipeline_stop_reason, Some(abort_reason));
    }

    #[test]
    fn empty_final_and_raw_text_returns_no_injection() {
        let outcome = PipelineOutcome::Completed {
            envelope: sample_envelope()
                .with_output_final_text("")
                .with_transcript_raw_text(""),
            trace: Vec::new(),
        };

        let route = Orchestrator::route_injection(&outcome);

        assert_eq!(route.target, InjectionTarget::None);
        assert_eq!(route.reason, InjectionRouteReason::NoInjectableText);
        assert_eq!(route.pipeline_stop_reason, None);
    }

    fn sample_envelope() -> MuninnEnvelopeV1 {
        MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:00:00Z")
    }

    fn sample_step_failed_reason() -> PipelineStopReason {
        PipelineStopReason::StepFailed {
            step_id: "stt".to_string(),
            failure: StepFailureKind::NonZeroExit,
            message: "step exited non-zero with status 9".to_string(),
        }
    }
}

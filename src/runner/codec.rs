use crate::config::{PipelineStepConfig, StepIoMode};
use crate::envelope::MuninnEnvelopeV1;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecodeDisposition {
    Normal,
    ContractBypass,
}

#[derive(Debug)]
pub(super) struct DecodedStepOutput {
    pub(super) envelope: MuninnEnvelopeV1,
    pub(super) disposition: DecodeDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodecErrorKind {
    SerializeInput,
    InvalidStdout,
    InvalidEnvelope,
}

#[derive(Debug)]
pub(super) struct CodecError {
    pub(super) kind: CodecErrorKind,
    pub(super) message: String,
}

pub(super) fn encode_step_input(
    step: &PipelineStepConfig,
    input_envelope: &MuninnEnvelopeV1,
) -> Result<Vec<u8>, CodecError> {
    match effective_io_mode(step) {
        StepIoMode::EnvelopeJson => {
            serde_json::to_vec(input_envelope).map_err(|source| CodecError {
                kind: CodecErrorKind::SerializeInput,
                message: format!("failed to serialize envelope for step input: {source}"),
            })
        }
        StepIoMode::TextFilter => Ok(current_text_for_filter(input_envelope).as_bytes().to_vec()),
        StepIoMode::Auto => unreachable!("effective_io_mode never returns Auto"),
    }
}

pub(super) fn decode_step_output(
    step: &PipelineStepConfig,
    input_envelope: MuninnEnvelopeV1,
    stdout: Vec<u8>,
    strict_step_contract: bool,
) -> Result<DecodedStepOutput, CodecError> {
    match effective_io_mode(step) {
        StepIoMode::EnvelopeJson => {
            decode_envelope_json_output(input_envelope, stdout, strict_step_contract)
        }
        StepIoMode::TextFilter => decode_text_filter_output(input_envelope, stdout),
        StepIoMode::Auto => unreachable!("effective_io_mode never returns Auto"),
    }
}

fn decode_envelope_json_output(
    input_envelope: MuninnEnvelopeV1,
    stdout: Vec<u8>,
    strict_step_contract: bool,
) -> Result<DecodedStepOutput, CodecError> {
    let output_value: Value = match serde_json::from_slice(&stdout) {
        Ok(value) => value,
        Err(_) if !strict_step_contract => return Ok(contract_bypass_output(input_envelope)),
        Err(source) => {
            return Err(CodecError {
                kind: CodecErrorKind::InvalidStdout,
                message: format!("step stdout was not valid JSON: {source}"),
            });
        }
    };

    if !output_value.is_object() {
        if strict_step_contract {
            return Err(CodecError {
                kind: CodecErrorKind::InvalidStdout,
                message: "step stdout JSON must be exactly one object".to_string(),
            });
        }

        return Ok(contract_bypass_output(input_envelope));
    }

    match serde_json::from_value::<MuninnEnvelopeV1>(output_value) {
        Ok(envelope) => Ok(DecodedStepOutput {
            envelope,
            disposition: DecodeDisposition::Normal,
        }),
        Err(_) if !strict_step_contract => Ok(contract_bypass_output(input_envelope)),
        Err(source) => Err(CodecError {
            kind: CodecErrorKind::InvalidEnvelope,
            message: format!("step JSON object was not a valid MuninnEnvelopeV1: {source}"),
        }),
    }
}

fn decode_text_filter_output(
    mut input_envelope: MuninnEnvelopeV1,
    stdout: Vec<u8>,
) -> Result<DecodedStepOutput, CodecError> {
    let output_text = String::from_utf8(stdout).map_err(|source| CodecError {
        kind: CodecErrorKind::InvalidStdout,
        message: format!("step stdout was not valid UTF-8 text: {source}"),
    })?;

    match text_filter_target(&input_envelope) {
        TextFilterTarget::OutputFinalText => input_envelope.output.final_text = Some(output_text),
        TextFilterTarget::TranscriptRawText => {
            input_envelope.transcript.raw_text = Some(output_text);
        }
    }

    Ok(DecodedStepOutput {
        envelope: input_envelope,
        disposition: DecodeDisposition::Normal,
    })
}

fn contract_bypass_output(envelope: MuninnEnvelopeV1) -> DecodedStepOutput {
    DecodedStepOutput {
        envelope,
        disposition: DecodeDisposition::ContractBypass,
    }
}

fn effective_io_mode(step: &PipelineStepConfig) -> StepIoMode {
    match step.io_mode {
        StepIoMode::Auto => StepIoMode::TextFilter,
        other => other,
    }
}

fn current_text_for_filter(envelope: &MuninnEnvelopeV1) -> &str {
    if let Some(text) = non_empty_text(&envelope.output.final_text) {
        return text;
    }
    if let Some(text) = non_empty_text(&envelope.transcript.raw_text) {
        return text;
    }
    ""
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextFilterTarget {
    OutputFinalText,
    TranscriptRawText,
}

fn text_filter_target(envelope: &MuninnEnvelopeV1) -> TextFilterTarget {
    if non_empty_text(&envelope.output.final_text).is_some() {
        TextFilterTarget::OutputFinalText
    } else if non_empty_text(&envelope.transcript.raw_text).is_some() {
        TextFilterTarget::TranscriptRawText
    } else {
        TextFilterTarget::OutputFinalText
    }
}

fn non_empty_text(text: &Option<String>) -> Option<&str> {
    text.as_deref().filter(|value| !value.is_empty())
}

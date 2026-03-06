use std::process::ExitCode;

use anyhow::Result;
use muninn::config::{PipelineStepConfig, StepIoMode};

use crate::{refine, stt_google_tool, stt_openai_tool};

const INTERNAL_STEP_MARKER: &str = "__internal_step";

pub fn maybe_handle_internal_step(args: &[String]) -> Option<ExitCode> {
    if args.len() < 3 || args[1] != INTERNAL_STEP_MARKER {
        return None;
    }

    let tool = canonical_tool_name(&args[2])?;
    Some(match tool {
        "stt_openai" => stt_openai_tool::run_as_internal_tool(),
        "stt_google" => stt_google_tool::run_as_internal_tool(),
        "refine" => refine::run_as_internal_tool(),
        _ => return None,
    })
}

pub fn rewrite_internal_tool_step(step: &mut PipelineStepConfig) -> Result<bool> {
    let Some(tool) = canonical_tool_name(&step.cmd) else {
        return Ok(false);
    };

    step.cmd = tool.to_string();
    step.io_mode = StepIoMode::EnvelopeJson;
    Ok(true)
}

pub fn is_transcription_step(step: &PipelineStepConfig) -> bool {
    matches!(
        step_tool_name(step),
        Some("stt_openai") | Some("stt_google")
    )
}

pub fn canonical_tool_name(raw: &str) -> Option<&'static str> {
    match raw {
        "stt_openai" => Some("stt_openai"),
        "stt_google" => Some("stt_google"),
        "refine" => Some("refine"),
        _ => None,
    }
}

fn step_tool_name(step: &PipelineStepConfig) -> Option<&'static str> {
    canonical_tool_name(&step.cmd).or_else(|| {
        let [marker, tool, ..] = step.args.as_slice() else {
            return None;
        };
        if marker != INTERNAL_STEP_MARKER {
            return None;
        }
        canonical_tool_name(tool)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};

    #[test]
    fn normalizes_internal_tool_step_to_canonical_builtin_name() {
        let mut step = PipelineStepConfig {
            id: "refine".to_string(),
            cmd: "refine".to_string(),
            args: vec!["--example".to_string()],
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(rewritten);
        assert_eq!(step.cmd, "refine");
        assert_eq!(step.args, vec!["--example"]);
        assert_eq!(step.io_mode, StepIoMode::EnvelopeJson);
    }

    #[test]
    fn leaves_external_command_unchanged() {
        let mut step = PipelineStepConfig {
            id: "uppercase".to_string(),
            cmd: "/opt/homebrew/bin/jq".to_string(),
            args: vec!["-c".to_string()],
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(!rewritten);
        assert_eq!(step.cmd, "/opt/homebrew/bin/jq");
        assert_eq!(step.args, vec!["-c"]);
        assert_eq!(step.io_mode, StepIoMode::Auto);
    }

    #[test]
    fn canonical_tool_name_accepts_only_current_builtin_names() {
        assert_eq!(canonical_tool_name("stt_openai"), Some("stt_openai"));
        assert_eq!(canonical_tool_name("stt_google"), Some("stt_google"));
        assert_eq!(canonical_tool_name("refine"), Some("refine"));
        assert_eq!(canonical_tool_name("muninn-stt-openai"), None);
        assert_eq!(canonical_tool_name("muninn-stt-google"), None);
        assert_eq!(canonical_tool_name("muninn-refine"), None);
    }
}

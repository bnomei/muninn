use std::process::ExitCode;

use anyhow::{Context, Result};
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

    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let mut args = vec![INTERNAL_STEP_MARKER.to_string(), tool.to_string()];
    args.extend(step.args.clone());
    step.cmd = current_exe.display().to_string();
    step.args = args;
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
    use std::path::PathBuf;

    #[test]
    fn rewrites_internal_tool_step_to_current_executable() {
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
        assert!(PathBuf::from(&step.cmd).is_absolute());
        assert_eq!(step.args[0], "__internal_step");
        assert_eq!(step.args[1], "refine");
        assert_eq!(step.args[2], "--example");
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
    fn legacy_aliases_are_not_rewritten() {
        let mut step = PipelineStepConfig {
            id: "legacy".to_string(),
            cmd: "muninn-stt-openai".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        };

        let rewritten = rewrite_internal_tool_step(&mut step).expect("rewrite should succeed");

        assert!(!rewritten);
        assert_eq!(step.cmd, "muninn-stt-openai");
    }
}

# Design — 02-core-engine

## Core components
- `EngineStateMachine`
- `ScoringGate`
- `Orchestrator` (audio -> pipeline -> inject)

## Traits
- `AudioRecorder`: start/stop/cancel; returns in-memory PCM.
- `AudioMaterializer`: writes temp WAV as needed.
- `PipelineExecutor`: wraps `muninn-pipeline` crate.
- `Injector`: keyboard text injection abstraction.
- `Indicator`: recording/processing state output.

## Threshold logic
- default thresholds: top >= 0.84, margin >= 0.10
- acronym/short span thresholds: top >= 0.90, margin >= 0.15

## Fallback order
- preferred: `output.final_text`
- fallback: `transcript.raw_text`
- failure: no injection + error event

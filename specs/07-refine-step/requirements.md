# Requirements — 07-refine-step

## Scope
Add one built-in LLM-backed pipeline step that minimally refines a transcript for developer dictation. The step must read `transcript.raw_text` plus `transcript.system_prompt`, preserve the raw transcript for fallback/replay, and write accepted refined output to `output.final_text`.

## EARS requirements
1. When `refine` receives an envelope with a non-empty `transcript.raw_text`, the system shall call the configured refine model with a fixed Muninn system contract and the configured `transcript.system_prompt` as hints.
2. When `refine` receives an envelope without a non-empty `transcript.raw_text`, the system shall return the envelope unchanged.
3. When the model returns an accepted refinement, the system shall write the refined text to `output.final_text` and shall leave `transcript.raw_text` unchanged.
4. When the model output is empty, excessively different from the raw transcript, or otherwise invalid, the system shall reject the refinement and shall return the envelope without mutating `transcript.raw_text` or `output.final_text`.
5. When refinement is rejected or the provider call fails, the step shall record a structured error entry in the envelope or emit a structured stderr error so the pipeline can continue or abort according to step policy.
6. The step shall prefer minimal corrections over rewriting, including preserving wording, order, and meaning unless an obvious technical correction is required.
7. The built-in system contract shall instruct the model to correct technical terms, developer tooling, commands, flags, file names, paths, env vars, acronyms, and obvious dictation errors while avoiding stylistic rewrites.
8. The config surface shall include refine model settings and acceptance thresholds without requiring a separate UI.
9. The default `transcript.system_prompt` shall steer Muninn toward minimal technical corrections and explicit abstention when uncertain.
10. The main app shall recognize `refine` as an internal pipeline tool resolved through the Muninn executable.
11. The main app shall recognize `stt_openai` and `stt_google` references as internal pipeline tools so built-in STT steps do not require separate sibling binaries in the active pipeline.

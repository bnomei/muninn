# Design — 18-scoring-runtime-wiring

## Overview
The repository already exposes threshold-based replacement decisions, but the tray runtime never consults them. The least invasive fix is to treat scoring as a post-pipeline envelope finalizer: if a step produced `uncertain_spans` plus `replacements`, Muninn derives `output.final_text` from `transcript.raw_text` before injection routing.

## Decisions
- Keep the current scoring library types and config surface.
- Parse `uncertain_spans` and `replacements` from the envelope as best-effort structured values.
- Group candidate replacements by original span text, use the highest-score candidate as the replacement proposal, and use all group scores for threshold/margin evaluation.
- Apply accepted replacements to `transcript.raw_text` using explicit span offsets when they are valid; otherwise leave the transcript unchanged.

## Non-goals
- No new provider behavior to populate replacements.
- No change to orchestrator fallback order beyond optionally supplying `output.final_text`.

## Validation strategy
- Unit tests for accepted, rejected, and invalid-span replacement application.
- `cargo test -q`

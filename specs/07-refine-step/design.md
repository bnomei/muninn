# Design — 07-refine-step

Historical note: this buildout spec captures the original refine-step milestone. Current implemented runtime behavior lives in `specs/08-current-runtime-surface/`.

## Step role
- Add an internal Muninn-backed pipeline step addressed as `refine`.
- The pipeline continues to see a normal step command, but the app resolves it to the current `muninn` executable with an internal subcommand.
- The internal subcommand reads one `MuninnEnvelopeV1` JSON object from stdin and writes one `MuninnEnvelopeV1` JSON object to stdout.
- It is a post-STT refinement layer, not a replacement for STT.

## Envelope behavior
- Input source:
  - `transcript.raw_text`
  - `transcript.system_prompt`
- Output target:
  - write accepted result to `output.final_text`
- Preserve:
  - `transcript.raw_text`
  - unrelated envelope fields

## Prompt contract
- Fixed built-in system contract carries the hard Muninn rules:
  - preserve meaning exactly
  - make the fewest possible changes
  - prefer no change over risky change
  - correct obvious technical terms and dictation mistakes
  - do not paraphrase, summarize, reorder, or add information
- `transcript.system_prompt` is appended as user/project hints, not the primary contract.
- `transcript.raw_text` is sent as the user message.

## Provider model
- Initial provider: OpenAI text model only.
- Config block:
  - `[refine]`
  - `provider = "openai"`
  - `endpoint = "https://api.openai.com/v1/chat/completions"`
  - `model = "gpt-4.1-mini"`
  - `temperature = 0.0`
  - `max_output_tokens = 512`
  - `max_length_delta_ratio = 0.25`
  - `max_token_change_ratio = 0.60`
  - `max_new_word_count = 2`
- API key resolution order:
  - `OPENAI_API_KEY`
  - `providers.openai.api_key`

## Acceptance gate
- Trim the returned text.
- Reject if empty.
- Accept unchanged output.
- Reject if:
  - absolute length delta exceeds `raw_len * max_length_delta_ratio`
  - token change ratio exceeds `max_token_change_ratio`
  - newly introduced token count exceeds `max_new_word_count`
- On rejection:
  - leave envelope text fields unchanged
  - append a structured error entry to `errors`
  - still return exit `0` so the pipeline can continue with the raw transcript

## Error model
- Hard failures:
  - invalid stdin JSON
  - missing credentials when provider call is required
  - HTTP failure
  - malformed provider response
- Hard failures exit non-zero with structured stderr JSON.
- Soft validation failures stay in-envelope as structured `errors`.

## Integration points
- Add `RefineConfig` to `AppConfig`.
- Update default `TranscriptConfig.system_prompt`.
- Add an internal refine subcommand to `src/main.rs`.
- Add `refine` to the app's internal step resolution so it maps to the current executable plus refine subcommand args.
- Add a sample pipeline step to `configs/config.sample.toml`.
- Update the live user config to insert `refine` after `stt_openai` and before example transforms.

## Non-goals
- No multi-provider refine routing in this change.
- No structured uncertain-span generation in this step.
- No mutation of `transcript.raw_text`.

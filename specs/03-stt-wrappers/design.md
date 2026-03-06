# Design — 03-stt-wrappers

## Wrapper approach
- Each STT tool is implemented as an internal Muninn subcommand:
  - pipeline step ref resolves to the `muninn` executable plus internal tool args
  - reads full envelope JSON from stdin
  - resolves credentials
  - sends multipart/file request to provider endpoint
  - updates transcript fields
  - writes full envelope JSON to stdout

## Shared helper module
- Helper functionality lives in `src/` local modules:
  - stdin envelope parse
  - wav path checks
  - envelope writeback
  - error formatting

## Provider specifics
- OpenAI:
  - key from `OPENAI_API_KEY` then config.
  - model default `gpt-4o-mini-transcribe` unless overridden.
- Google:
  - token/key from env then config.
  - endpoint/model in config.

## Testing
- unit tests for credential precedence and envelope preservation.
- HTTP behavior mocked via local test server where possible.

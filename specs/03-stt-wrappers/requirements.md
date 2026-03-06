# Requirements — 03-stt-wrappers

Historical note: this buildout spec captures the original STT wrapper milestone. Current implemented runtime behavior lives in `specs/08-current-runtime-surface/`.

## Scope
Implement built-in OpenAI and Google STT internal tools that obey the pipeline envelope contract.

## EARS requirements
1. When an internal STT tool receives envelope JSON with `audio.wav_path`, the system shall call the configured provider and populate `transcript.raw_text` on success.
2. If provider credentials are unavailable, then the internal STT tool shall exit non-zero with structured error in stderr.
3. If provider call fails, then the internal STT tool shall preserve input envelope and return error details.
4. The internal STT tools shall preserve all unrelated envelope fields.
5. Where env and config credentials both exist, the internal STT tools shall prioritize env.
6. The internal STT tools shall support provider model/endpoint overrides from config.

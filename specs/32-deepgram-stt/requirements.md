# Requirements — 32-deepgram-stt

## Scope
Add a Deepgram-backed STT built-in step so Muninn has the preferred cloud transcription option in the default ordered provider route without changing the envelope contract.

## EARS requirements
1. When the Deepgram backend is selected and credentials are available, the system shall expose a built-in step that transcribes the recorded audio through Deepgram’s STT API.
2. When the backend is configured with no explicit model, the system shall use a documented default model choice appropriate for general-purpose Muninn dictation.
3. When the backend transcribes successfully, the system shall write the best transcript to `transcript.raw_text` while preserving unrelated envelope fields.
4. When credentials are missing or invalid, the system shall emit actionable diagnostics and classify the outcome consistently with the ordered route-continuation model introduced by spec 29.
5. When the backend request fails or Deepgram returns no usable transcript, the system shall preserve structured failure details without losing the original envelope.
6. When Muninn ships the default ordered provider route, the system shall prefer Deepgram ahead of OpenAI and Google inside the cloud segment.
7. When documentation is updated for this backend, the system shall explain where Deepgram fits relative to the local-first default route and which baseline model/configuration Muninn uses.

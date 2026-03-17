# Requirements — 30-whisper-cpp-backend

## Scope
Add a portable offline Whisper backend built on `whisper.cpp`-compatible inference so Muninn can transcribe completed recordings locally across more environments than the Apple-native backend alone.

## EARS requirements
1. When the Whisper backend is selected and local model assets are available, the system shall expose a built-in step that transcribes recorded audio without network provider credentials.
2. When the Whisper backend is configured with no explicit model choice, the system shall support a documented launchable default model strategy for first use.
3. When the Whisper backend runs, the system shall process completed recordings only and shall not require or expose streaming or partial-transcription behavior.
4. When the backend runs on Apple Silicon or other supported accelerators, the system shall prefer the documented accelerated path where available and fall back safely when it is not.
5. When the backend transcribes successfully, the system shall write the best transcript to `transcript.raw_text` while preserving unrelated envelope fields.
6. When required local model assets are missing, the system shall emit an actionable missing-model diagnostic or perform the documented managed-model install flow.
7. When the backend is used inside an ordered provider route, the system shall classify missing-model, unsupported-build, and runtime-failure outcomes in the normalized route-continuation model introduced by spec 29.
8. When documentation is updated for this backend, the system shall explain the local model lifecycle, the no-streaming boundary, and the storage/performance tradeoffs of the supported model choices.

# Requirements — 31-apple-speech-transcriber

## Scope
Add an Apple-native on-device STT backend built on the modern Speech framework so Muninn can transcribe completed recordings locally on supported macOS 26+ systems without provider credentials.

## EARS requirements
1. When the Apple speech backend is selected on supported macOS 26+ systems, the system shall expose a built-in step that transcribes the recorded audio fully on device.
2. When the Apple speech backend is selected on an unsupported platform or on macOS versions below 26, the system shall emit an actionable unavailable-backend diagnostic instead of failing silently.
3. When the backend requires Speech framework assets that are not yet installed, the system shall attempt the supported asset-install flow or emit an actionable installation diagnostic.
4. When the backend transcribes successfully, the system shall write the best transcript to `transcript.raw_text` while preserving unrelated envelope fields.
5. When the backend runs, the system shall process completed recordings only and shall not require or expose streaming or partial-transcription behavior.
6. When the backend is used inside an ordered provider route, the system shall classify unsupported-platform and unavailable-assets outcomes in the normalized route-continuation model introduced by spec 29.
7. When the backend is configured, the system shall support locale selection and reasonable defaults for the current user locale where the platform API supports it.
8. When the backend is unavailable or returns no transcript, the system shall preserve structured failure details without requiring provider credentials.
9. When documentation is updated for this backend, the system shall explain the macOS 26+ version gate and the fact that Apple manages the underlying speech assets outside the Muninn bundle.

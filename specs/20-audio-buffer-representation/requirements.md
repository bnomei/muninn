# Requirements — 20-audio-buffer-representation

## Scope
Reduce the live memory footprint of buffered microphone capture.

## EARS requirements
1. While recording is active, the system shall buffer capture samples in a representation no larger than 16-bit PCM per sample.
2. When the buffered representation changes, the system shall preserve the existing overflow cap semantics.
3. When recording stops, the system shall still emit the configured 16-bit PCM WAV output format.
4. If a device sample format requires normalization, then the system shall convert into the buffered representation without changing recorder behavior.

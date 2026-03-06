# Requirements — 02-core-engine

## Scope
Implement runtime orchestration, mode/state transitions, scoring gate policy, and fallback routing.

## EARS requirements
1. While the app is idle, the system shall accept `push-to-talk` hold and `done` toggle events.
2. When PTT key is pressed, the system shall enter `RecordingPushToTalk`; when released, it shall stop recording and enter processing.
3. When Done toggle is pressed while idle, the system shall enter `RecordingDone`; when pressed again, it shall stop recording and enter processing.
4. If cancel is triggered while recording, then the system shall discard capture and return to idle.
5. While processing or injecting, the system shall ignore new record triggers.
6. If pipeline outcome is fallback-required and raw transcript exists, then the system shall inject raw transcript.
7. If replacement candidates are ambiguous, then the system shall preserve original span text.
8. The system shall apply stricter thresholds for acronym or short spans.

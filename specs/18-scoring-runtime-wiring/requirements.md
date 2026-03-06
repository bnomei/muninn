# Requirements — 18-scoring-runtime-wiring

## Scope
Make the existing scoring/replacement library and config surface affect the active tray runtime in a concrete, testable way.

## EARS requirements
1. When a pipeline outcome contains replacement candidates and uncertain spans plus `transcript.raw_text`, the system shall apply scoring thresholds before deciding whether to materialize `output.final_text`.
2. If replacement candidates are ambiguous under the configured thresholds, then the system shall preserve the original transcript text for that span.
3. If the runtime cannot safely apply scored replacements from the envelope data, then the system shall leave the envelope unchanged.
4. When scoring is wired into the tray runtime, the system shall preserve existing injection fallback order.

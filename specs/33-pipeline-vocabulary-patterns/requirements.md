# Requirements — 33-pipeline-vocabulary-patterns

## Scope
Document and validate a pipeline-first vocabulary pattern so Muninn users can bias refinement toward domain terms with the existing prompt surfaces, without introducing a dedicated provider-vocabulary subsystem.

## EARS requirements
1. When documentation is updated for this feature, the system shall describe a bounded vocabulary-JSON pattern that can be embedded or appended through the existing transcript/refine prompt surfaces.
2. When sample config or example snippets are shipped, the system shall show how to use the existing pipeline and profile/voice overlays to apply vocabulary hints without requiring a dedicated `[vocabulary]` config section.
3. When the documented pattern is used, the system shall preserve current STT and refine behavior for users who do not opt in to vocabulary hints.
4. If a code change is required to make the pattern usable, the system shall add only a generic prompt-composition or append helper rather than a provider-specific vocabulary feature surface.
5. When examples or tests are added for this feature, the system shall verify that the documented pattern reaches the refine path while preserving the existing pipeline contract.
6. When documentation is updated for this feature, the system shall explain that provider-native vocabulary/adaptation remains out of scope and any backend-specific biasing is best-effort through existing prompt surfaces only.

# Requirements — 17-pipeline-envelope-churn

## Scope
Reduce whole-envelope allocation churn in the pipeline runner while preserving deadline, timeout, trace, and failure-policy behavior.

## EARS requirements
1. When a pipeline step completes successfully in text-filter mode, the system shall update the owned envelope in place instead of cloning the full envelope first.
2. When a non-strict envelope-json step emits invalid stdout, the system shall preserve the current envelope without allocating an extra cloned copy.
3. When a step fails before yielding a replacement envelope, the system shall return the current owned envelope to the caller without changing fallback or abort semantics.
4. When pipeline behavior is unchanged after the refactor, the system shall keep existing step-runner tests green.

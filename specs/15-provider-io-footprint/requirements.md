# Requirements — 15-provider-io-footprint

## Scope
Reduce per-request memory churn in built-in STT/refine tools by streaming or single-buffering provider requests and by avoiding textual stdin/stdout copies of the envelope contract.

## EARS requirements
1. When the OpenAI STT tool uploads audio, the system shall stream the WAV file from disk instead of reading the entire file into a multipart byte buffer first.
2. When the Google STT tool uploads audio, the system shall build the JSON request body without holding both a full raw-audio buffer and a serialized `serde_json::Value` for the same request.
3. When internal STT/refine tools receive an envelope on stdin, the system shall deserialize directly from stdin instead of copying the full JSON payload into an intermediate `String`.
4. When internal STT/refine tools emit an envelope on stdout, the system shall serialize directly to stdout instead of building a redundant output `String`.
5. When built-in STT/refine tools perform HTTP requests, the system shall reuse a process-local HTTP client.

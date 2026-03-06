# Requirements — 14-audio-capture-footprint

## Scope
Reduce recording-side memory and stop-path latency by using lower-footprint capture settings when supported and by avoiding whole-buffer post-processing copies.

## EARS requirements
1. When the configured recording sample rate and channel count are supported by the selected input device, the system shall capture using a matching low-footprint input config instead of always using the device default.
2. If the selected input device does not support the configured recording sample rate or mono capture, then the system shall fall back to a supported config and still emit the configured WAV output format.
3. When recording stops, the system shall write the configured WAV output without materializing avoidable full-buffer intermediate audio vectors.
4. When recording configuration changes at runtime, the system shall apply the new capture preference on the next engine initialization without regressing current recorder behavior.

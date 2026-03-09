---
id: coreml-apple-silicon-embedding
outcome: agent-alignment
title: CoreML EP does NOT accelerate BGE-small — ops fall back to CPU with 5x memory
---

Tested ort CoreML execution provider with BGE-small-en-v1.5. CoreML registered and discovered Apple ANE/GPU/CPU devices, but the transformer attention and layer norm ops are not supported by the ONNX→CoreML automatic conversion. All ops fell back to CPU with 5.4GB memory usage (vs ~1GB baseline), no speedup, and OOM risk.

Getting ANE acceleration requires a pre-compiled CoreML .mlpackage model, not automatic ONNX conversion. The GPU machine or embedding API is the right path for acceleration.

Previous metis entry was speculative — this one is validated.

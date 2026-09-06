---
title: CUDA attestation must follow the retained production session
outcome: context-assembly
---

Provider registration and a finite output vector do not establish CUDA execution.
ONNX Runtime can partition work onto CPU after successful CUDA registration.
Profile the same session retained for production encoding and reject CPU floating
compute, while permitting bounded integer shape plumbing. An unrelated probe
session cannot attest the encoder that actually writes the vectors.

Generation and query encoders must also agree on effective provider, ordinal,
actual model/tokenizer bytes, and preprocessing. Configured `auto` is a policy,
not a vector-space identity. Pin the generation database and manifest together
before querying so publication cannot mix snapshots.

Hardware regression evidence on NUC14 (RTX 3060 Ti, f32, TF32 disabled): retained
MiniLM session profile observed CUDA MatMul/FusedMatMul/LayerNormalization/Softmax;
CPU placement was limited to integer Gather/Unsqueeze. Nine canonical production
function inputs extracted from `src/embed/config.rs` produced finite 384-element
vectors. This is development verification, not a substitute for matching CI
artifact installation and real MCP delivery verification.

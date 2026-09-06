---
id: openvino-runtime-identity-is-bundle-local
outcome: context-assembly
title: OpenVINO attestation must describe the provider-loaded bundle
---

The host OpenVINO Python package and the ONNX Runtime OpenVINO provider can load
different runtime versions. On NUC14, the verified ORT OpenVINO 1.24.1 bundle
reported OpenVINO 2025.4.1-0-test and Intel(R) Arc(TM) Graphics (iGPU) at GPU.0,
even though standalone OpenVINO 2026.3.1 was obtainable. A host-only device smoke
cannot satisfy the RNA encoder's runtime-version acceptance criterion.

For RNA's Arc encoder, query the C API from the provider's bundle, reject loaded
OpenVINO libraries from other directories, and hash the runtime bundle plus the
exact ONNX/tokenizer bytes passed into the retained encoder. Persist the resolved
provider, device, requested precision, fallback policy and cause with generation
identity. Pin query validation to the same generation snapshot as its table.

Finite output and a device name are distinct from kernel-execution and precision
evidence. The real-source development probe produces a finite 384-vector, but
does not by itself prove every operator's placement or internal precision.
Published-generation diagnostics likewise do not attest a newly initialized query
encoder. Final delivery still requires a matching CI artifact and real MCP client.

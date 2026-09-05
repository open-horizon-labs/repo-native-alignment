---
id: openvino-runtime-version-is-provider-local
outcome: context-assembly
status: candidate
---

# OpenVINO runtime evidence belongs to the loaded provider

PR #870's configured ONNX Runtime 1.24.1 wheel loads its bundled OpenVINO
2025.4.1 runtime even when a separate host environment supplies OpenVINO 2026.3.1.
Query the C runtime beside the selected ORT library when recording version and
GPU.0 FULL_DEVICE_NAME. A host-only OpenVINO smoke test does not identify the
runtime executing RNA embeddings.

On NUC14, strace resolves the PCI render symlink to renderD128 and records
successful DRM_IOCTL_I915_GEM_EXECBUFFER2 calls on that file descriptor. Combine
this with i915 and PCI vendor 0x8086; an ordinal in RNA's log is not independent
proof of Intel execution. f32 output vectors do not establish internal precision.

A missing ORT path hung under ort rc.12. Upgrade the coupled fastembed/ort pair
and test error paths in a bounded subprocess because ORT state is process-global.
Repeated failed loading under rc.13 also aborted in testing; retaining the first
initialization error avoids re-entering partial ORT state.

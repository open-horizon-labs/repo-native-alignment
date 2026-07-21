# SWE-bench context packets

`scripts/swebench_context_packets.py` builds the frozen B (RNA) and C
(RNA + structural ActContext) packets without contacting an LSP, scanning,
encoding repository vectors, repairing a cache, or calling a model API.

## Inputs

Use one exact row from the frozen SWE-bench dataset and the matching immutable,
verifier-clean combined cache produced by issue #786. The RNA binary and
artifact receipt must come from that cache's successful CI artifact. Copy the
protocol bundle, artifact receipt, GitHub artifact, cache archive, cache
sidecar, and cache core digests from their immutable receipts; do not infer or
recompute trust anchors from an unverified input.

Run with the frozen Python/tool lock. Packet token accounting requires
`tiktoken==0.13.0` and the pinned `cl100k_base` ranks.

## Build and self-verify

```sh
python3 scripts/swebench_context_packets.py build \
  --checkout /absolute/path/to/exact-instance-checkout \
  --dataset-row /absolute/path/to/exact-instance-row.json \
  --cache-archive /absolute/path/to/combined-cache.tar.gz \
  --cache-manifest /absolute/path/to/combined-cache.manifest.json \
  --artifact-receipt /absolute/path/to/artifact-verification-receipt.json \
  --rna-binary /absolute/path/to/verified-ci-artifact/repo-native-alignment \
  --output /new/absent/output/path \
  --expected-digest PROTOCOL_BUNDLE_SHA256 \
  --expected-artifact-receipt-digest ARTIFACT_RECEIPT_FILE_SHA256 \
  --expected-artifact-head-sha ARTIFACT_GIT_COMMIT \
  --expected-github-artifact-digest sha256:GITHUB_ARTIFACT_DIGEST \
  --expected-cache-archive-sha256 CACHE_ARCHIVE_SHA256 \
  --expected-cache-sidecar-sha256 CACHE_SIDECAR_SHA256 \
  --expected-cache-core-sha256 CACHE_CORE_SHA256
```

The output path must not exist. Success prints a JSON object with
`status: "ready"`, the frozen instance ID, and the manifest SHA-256. Failure is
fail-closed; retain the output and checkout as evidence.

## Independent offline verification

Run verification from the same exact producer commit and frozen Python/tool
lock. Supply the same external trust anchors rather than trusting values inside
the packet directory.

```sh
python3 scripts/swebench_context_packets.py verify \
  --root /absolute/path/to/packet-output \
  --dataset-row /absolute/path/to/exact-instance-row.json \
  --expected-digest PROTOCOL_BUNDLE_SHA256 \
  --expected-artifact-receipt-digest ARTIFACT_RECEIPT_FILE_SHA256 \
  --expected-artifact-head-sha ARTIFACT_GIT_COMMIT \
  --expected-github-artifact-digest sha256:GITHUB_ARTIFACT_DIGEST \
  --expected-cache-archive-sha256 CACHE_ARCHIVE_SHA256 \
  --expected-cache-sidecar-sha256 CACHE_SIDECAR_SHA256 \
  --expected-cache-core-sha256 CACHE_CORE_SHA256
```

Verification closes the evidence inventory, replays the exact command
protocol from retained stdout/stderr, reconstructs both packets, rechecks locus
expressibility, and confirms the injected cache was unchanged. It performs no
scan, LSP request, corpus encoding, cache repair, or model/API call.

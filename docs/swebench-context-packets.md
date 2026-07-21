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

## Provision the packet runtime

Provision this environment before entering the offline packet phase. The lock
was generated for CPython 3.12 on macOS arm64 with `uv 0.9.18`; every direct
and transitive distribution is exact-pinned and hash-checked.

```sh
uv venv --python 3.12 /absolute/path/to/packet-venv
uv pip sync \
  --python /absolute/path/to/packet-venv/bin/python \
  --require-hashes \
  scripts/requirements/swebench-context-packets.lock
TIKTOKEN_CACHE_DIR=/absolute/path/to/tiktoken-cache \
  /absolute/path/to/packet-venv/bin/python -c \
  "import tiktoken; assert tiktoken.__version__ == '0.13.0'; tiktoken.get_encoding('cl100k_base')"
```

Before disabling network, require the sole regular file in
`/absolute/path/to/tiktoken-cache` to have SHA-256
`223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7`.
That is the frozen `cl100k_base` mergeable-ranks digest. Keep
`TIKTOKEN_CACHE_DIR` set to this verified cache for build and verification;
neither command then needs network access.

## Build and self-verify

```sh
TIKTOKEN_CACHE_DIR=/absolute/path/to/tiktoken-cache \
/absolute/path/to/packet-venv/bin/python scripts/swebench_context_packets.py build \
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
TIKTOKEN_CACHE_DIR=/absolute/path/to/tiktoken-cache \
/absolute/path/to/packet-venv/bin/python scripts/swebench_context_packets.py verify \
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

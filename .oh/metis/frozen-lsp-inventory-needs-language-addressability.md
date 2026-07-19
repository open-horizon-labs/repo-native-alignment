---
artifact_type: metis
title: Frozen LSP inventory needs an explicit language-addressability boundary
outcome: context-assembly
phase: problem-space
status: candidate
issue: 785
created: 2026-07-17
---

# Frozen LSP inventory needs an explicit language-addressability boundary

The exact N=70 inventory contains 269,974 tracked file observations. Existing
binary, generated, vendor, and configured-policy rules exclude 54,877, but the
per-file LSP gate still treats 215,097 files across 143 suffix buckets as
language-addressable. That set includes real Python, Cython, C/C++, Markdown,
RST, and config code, but it also includes arbitrary text, numeric fixtures,
certificates, font metrics, negative-test unknown extensions, and
application-specific data.

A directory role such as `tests/` does not establish that every descendant is
test *code*. Conversely, an unknown extension is not evidence that a file is
safe to ignore. Fail-closed completeness needs two independent decisions:

1. Retain and classify every tracked path with a deterministic reason.
2. Require LSP capability evidence only when the format is genuinely
   language-addressable; unknown or ambiguous files remain blocking.

Provisioning cannot repair a missing semantic boundary. Mapping data fixtures
to an unrelated generic server would turn process liveness into false code
intelligence. Blanket test/docs exclusions would hide real source. The policy
must therefore use an explicit closed taxonomy with adversarial regressions
that prevent source, docs, config, or test code from being relabeled as data.

Frozen evidence: population SHA-256
`067a5589b4cdb34c5fbd81bb6ff7ff6ede4dbfc26694758fafbef3544f9e6acf`,
inventory digest
`90c8c88a113bc5fa53a4a3ab233f4131901a60386dc2b78470e6698d2a504edd`.
Blocker: GitHub #801.

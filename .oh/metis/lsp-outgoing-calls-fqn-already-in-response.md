---
id: lsp-outgoing-calls-fqn-already-in-response
outcome: agent-alignment
title: LSP outgoing-calls response already contains FQN — no extra hover round-trip needed
---

## What Happened

PR #43 (LSP-as-extractor: synthesize virtual external nodes from hover responses) was designed around the assumption that hover responses would be needed to get fully-qualified names for call targets. During implementation, it was discovered that `callHierarchy/outgoingCalls` responses already include FQN in `call["to"]["detail"]`.

## The Fix

Used `call["to"]["detail"]` directly instead of issuing a `textDocument/hover` request for each call site. This eliminates one round-trip per call target.

## Implication

Before assuming an extra LSP request is needed, check what the current response already returns. LSP responses often include richer metadata than the primary use case requires. The `detail` field in call hierarchy items was designed exactly for this — it carries the container name / FQN.

## Broader Pattern

When integrating a protocol-based tool (LSP, DAP, language servers in general): read the response schema fully before designing round-trip reduction strategies. The redundancy is often already there.

## Evidence Source

PR #43, `callHierarchy/outgoingCalls` response inspection during LSP-as-extractor implementation.

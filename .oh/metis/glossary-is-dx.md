---
id: glossary-is-dx
outcome: agent-alignment
title: Every unfamiliar term in the README is a bounce point
---

## The Learning

"Metis" means nothing to someone who hasn't read Greek philosophy. "Tree-sitter" means nothing to someone who hasn't done language tooling. "Embeddings" means nothing to someone who hasn't done ML. "LanceDB" and "petgraph" are implementation details that look like requirements.

Every unfamiliar term in the README is a point where a potential user decides "this isn't for me." The glossary isn't documentation — it's the difference between "I understand what this does" and closing the tab.

## What We Did

Added a full glossary covering: tree-sitter, LSP, graph, embeddings, LanceDB, petgraph, MCP, and all four business artifact types (outcome, signal, guardrail, metis). Each entry is one sentence of what it is, one sentence of why RNA uses it.

## The Principle

If you use a term that requires domain knowledge to understand, you owe the reader a definition. Not a link to Wikipedia — a definition in context, right there, in your README. The cost of a glossary is 10 minutes of writing. The cost of not having one is every user who bounced because "embeddings" sounded like ML research they don't do.

This applies beyond README: AGENTS.md tool descriptions, error messages, CLI help text. Anywhere a human or agent encounters your vocabulary for the first time.

---
id: subsystem-detection
status: proposed
---

# Subsystem Detection

Automatically detect subsystem boundaries in the code graph using community detection algorithms (e.g., Louvain, label propagation). A codebase with ~1.7 edges per node implies dense local clusters — modules that are tightly connected internally but sparsely connected externally. Surfacing these clusters lets agents reason about subsystems rather than individual files, which is closer to how engineers think.

## Desired behavior change

Agents retrieve and reason about **subsystems** ("the embedding pipeline", "the LSP enrichment layer", "the scanner") instead of listing individual functions. When asked "what does the embedding system do?", the agent gets the cluster boundary and its external interfaces, not 50 unrelated search results.

## Primary surface: repo_map

`repo_map` is the cold-start orientation tool — the first thing an agent sees when entering an unfamiliar codebase. Today it shows top symbols by PageRank and hotspot files. With subsystem detection, it shows the **architecture**: clusters of tightly-connected symbols with their boundary interfaces. This is what an engineer draws on a whiteboard when onboarding someone.

## Success signals

- `repo_map` shows detected subsystems with internal symbol count, edge density, and external interface functions
- `search(mode="impact")` can report "this change affects the scanner subsystem" not just a list of functions
- Agents spontaneously use subsystem names in their reasoning
- New developers using RNA orient faster because they see the system's architecture, not a flat list of important functions

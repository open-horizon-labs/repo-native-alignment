---
id: tool-consolidation-shipped
outcome: agent-alignment
title: 'Tool consolidation: 20 → 9 intent-based tools reduces agent confusion'
---

Consolidated 20 MCP tools to 9 intent-based tools. Key insight: the model is a parameter, the tool presentation is the variable. 20 tools means agents scan descriptions, pick wrong ones, or fall back to grep. 9 tools with clear intent-based names means agents pick correctly without instructions.

The consolidation pattern: server routes internally. oh_record handles all write operations via type param. graph_query handles all traversal via mode param. oh_search_context handles all search via include_code/include_markdown flags.

Delete, don't deprecate. Old tool names return unknown tool error.

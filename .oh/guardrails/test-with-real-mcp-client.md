---
id: test-with-real-mcp-client
outcome: agent-alignment
severity: soft
statement: Test MCP server changes with the official TypeScript SDK client or Claude Code, not just curl/pipe tests
---

Protocol negotiation, session handling, and error propagation differ between raw HTTP/stdio tests and real MCP clients. The protocol version bug (45 minutes lost) was invisible to curl but immediately fatal with the real client.

## Override Protocol
Skip only for changes that don't touch server initialization, transport, or tool registration.

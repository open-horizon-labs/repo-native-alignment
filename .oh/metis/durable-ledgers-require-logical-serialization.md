---
id: durable-ledgers-require-logical-serialization
outcome: context-assembly
title: 'Durable ledgers require logical serialization'
---

Atomic rename prevents torn JSON, but it does not prevent two writers from loading the same old store and replacing each others logical updates. A durable repo-local ledger needs both a same-process async lock and a cross-process ownership lock around merge-and-write.

Operation evidence must be captured when the operation actually starts. Attaching queue snapshots while constructing a report can predate the ledger and produce an empty historical record even though the live queue is correct.

The delivery check belongs at the real MCP client boundary: seed a schema-versioned live queue, call `list_roots` through the official TypeScript SDK, and assert the queue heading and in-flight phase are visible. Unit tests and direct renderers do not prove agent delivery.

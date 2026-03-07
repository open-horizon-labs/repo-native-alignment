import process from "node:process";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const [serverPath, repoPath] = process.argv.slice(2);
if (!serverPath || !repoPath) {
  console.error("Usage: node .github/scripts/mcp-smoke.mjs <server-path> <repo-path>");
  process.exit(2);
}

const client = new Client(
  { name: "rna-ci-smoke", version: "0.1.0" },
  { capabilities: {} },
);

const transport = new StdioClientTransport({
  command: serverPath,
  args: ["--repo", repoPath],
});

try {
  await client.connect(transport);
  const tools = (await client.listTools()).tools ?? [];
  if (tools.length === 0) throw new Error("Smoke check failed: zero tools");

  const mustHave = new Set(["oh_get_context", "oh_record", "outcome_progress"]);
  const seen = new Set(tools.map((t) => t.name));
  for (const name of mustHave) {
    if (!seen.has(name)) throw new Error(`Smoke check failed: missing tool '${name}'`);
  }

  console.log(`Smoke check passed: ${tools.length} tools visible.`);
} finally {
  await client.close();
}

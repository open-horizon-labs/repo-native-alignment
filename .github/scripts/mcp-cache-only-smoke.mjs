import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const [endpoint, repoPath, query = "main"] = process.argv.slice(2);
if (!endpoint || !repoPath) {
  console.error(
    "Usage: node .github/scripts/mcp-cache-only-smoke.mjs <endpoint> <repo> [query]",
  );
  process.exit(2);
}

const cachePath = path.join(repoPath, ".oh", ".cache");
const rounds = 5;
const clientCount = 3;

function snapshotTree(root) {
  const rows = [];
  const visit = (directory) => {
    for (const name of fs.readdirSync(directory).sort()) {
      const absolute = path.join(directory, name);
      const relative = path.relative(root, absolute);
      const stat = fs.lstatSync(absolute, { bigint: true });
      if (stat.isDirectory()) {
        visit(absolute);
      } else if (stat.isFile()) {
        rows.push({
          path: relative,
          size: stat.size.toString(),
          mtimeNs: stat.mtimeNs.toString(),
          sha256: createHash("sha256")
            .update(fs.readFileSync(absolute))
            .digest("hex"),
        });
      } else {
        rows.push({
          path: relative,
          kind: stat.isSymbolicLink() ? "symlink" : "other",
          target: stat.isSymbolicLink() ? fs.readlinkSync(absolute) : null,
          mtimeNs: stat.mtimeNs.toString(),
        });
      }
    }
  };
  visit(root);
  return rows;
}

async function connectClient(index) {
  const client = new Client({ name: `rna-cache-only-${index}`, version: "1.0.0" });
  const transport = new StreamableHTTPClientTransport(new URL(endpoint));
  await client.connect(transport);
  return { client, transport };
}

async function timedSearch(client) {
  const started = performance.now();
  const result = await client.callTool({
    name: "search",
    arguments: {
      query,
      search_mode: "strict",
      rerank: true,
      compact: true,
      limit: 5,
    },
  });
  const elapsedMs = performance.now() - started;
  if (result.isError) {
    throw new Error(`search returned MCP error: ${JSON.stringify(result.content)}`);
  }
  const text = (result.content ?? [])
    .filter((item) => item.type === "text")
    .map((item) => item.text ?? "")
    .join("\n");
  if (text.length === 0 || text.includes("STRICT SEMANTIC FAILURE")) {
    throw new Error(`search did not return qualified results: ${text.slice(0, 500)}`);
  }
  return elapsedMs;
}

if (!fs.statSync(cachePath).isDirectory()) {
  throw new Error(`cache directory is missing: ${cachePath}`);
}

const before = snapshotTree(cachePath);
const clients = await Promise.all(
  Array.from({ length: clientCount }, (_, index) => connectClient(index)),
);

// One explicit warmup initializes the resident encoder and reranker.
const warmupMs = await timedSearch(clients[0].client);
const timings = [];
for (let round = 0; round < rounds; round += 1) {
  timings.push(
    ...(await Promise.all(clients.map(({ client }) => timedSearch(client)))),
  );
}

await Promise.all(clients.map(({ client }) => client.close()));
const after = snapshotTree(cachePath);
if (JSON.stringify(after) !== JSON.stringify(before)) {
  throw new Error("cache-only MCP queries changed cache bytes or mtimes");
}

const sorted = [...timings].sort((left, right) => left - right);
const p95Ms = sorted[Math.ceil(sorted.length * 0.95) - 1];
const maxMs = sorted.at(-1);
if (p95Ms >= 2_000 || maxMs >= 10_000) {
  throw new Error(
    `cache-only latency exceeded contract: p95=${p95Ms.toFixed(1)}ms max=${maxMs.toFixed(1)}ms`,
  );
}

console.log(
  JSON.stringify(
    {
      endpoint,
      query,
      warmupMs,
      requests: timings.length,
      p95Ms,
      maxMs,
      cacheFiles: before.length,
      cacheUnchanged: true,
    },
    null,
    2,
  ),
);

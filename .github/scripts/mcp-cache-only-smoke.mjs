import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { createHash } from "node:crypto";
import { performance } from "node:perf_hooks";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const args = process.argv.slice(2);
const snapshotMode = args[0] === "--snapshot";
const verifySnapshotMode = args[0] === "--verify-snapshot";
const [endpoint, repoPath, query = "main", serverLogPath, baselinePath] =
  snapshotMode || verifySnapshotMode
    ? [undefined, args[1], undefined, undefined, args[2]]
    : args;
const snapshotExtraRoots = snapshotMode ? args.slice(2) : [];
if (
  (!snapshotMode && !verifySnapshotMode && (!endpoint || !repoPath)) ||
  ((snapshotMode || verifySnapshotMode) && !repoPath) ||
  (verifySnapshotMode && !baselinePath)
) {
  console.error(
    "Usage: node .github/scripts/mcp-cache-only-smoke.mjs <endpoint> <repo> [query] [server-log] [baseline-json]\n" +
      "   or: node .github/scripts/mcp-cache-only-smoke.mjs --snapshot <repo> [immutable-root...]\n" +
      "   or: node .github/scripts/mcp-cache-only-smoke.mjs --verify-snapshot <repo> <baseline-json>",
  );
  process.exit(2);
}

const cachePath = path.resolve(repoPath, ".oh", ".cache");
const rounds = 20;
const clientCount = 3;

function snapshotTree(root) {
  const rootStat = fs.lstatSync(root, { bigint: true });
  const rows = [
    {
      path: ".",
      kind: "directory",
      mtimeNs: rootStat.mtimeNs.toString(),
    },
  ];
  const visit = (directory) => {
    for (const name of fs.readdirSync(directory).sort()) {
      const absolute = path.join(directory, name);
      const relative = path.relative(root, absolute);
      const stat = fs.lstatSync(absolute, { bigint: true });
      if (stat.isDirectory()) {
        rows.push({
          path: relative,
          kind: "directory",
          mtimeNs: stat.mtimeNs.toString(),
        });
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

function snapshotState(extraRoots = []) {
  const roots = [
    { label: "cache", root: cachePath },
    ...extraRoots.map((root, index) => ({
      label: `immutable-${index + 1}`,
      root: path.resolve(root),
    })),
  ];
  if (new Set(roots.map(({ root }) => root)).size !== roots.length) {
    throw new Error("immutable snapshot roots must be unique");
  }
  return {
    schema: "rna-cache-only-immutable-snapshot-v1",
    roots: roots.map(({ label, root }) => {
      if (!fs.statSync(root).isDirectory()) {
        throw new Error(`immutable snapshot root is missing: ${root}`);
      }
      return { label, root, entries: snapshotTree(root) };
    }),
  };
}

function observeState(baseline) {
  if (
    baseline?.schema !== "rna-cache-only-immutable-snapshot-v1" ||
    !Array.isArray(baseline.roots) ||
    baseline.roots.length === 0
  ) {
    throw new Error("immutable snapshot baseline has an invalid schema");
  }
  return {
    schema: baseline.schema,
    roots: baseline.roots.map(({ label, root }) => ({
      label,
      root,
      entries: snapshotTree(root),
    })),
  };
}

if (!fs.statSync(cachePath).isDirectory()) {
  throw new Error(`cache directory is missing: ${cachePath}`);
}

if (snapshotMode) {
  process.stdout.write(
    `${JSON.stringify(snapshotState(snapshotExtraRoots), null, 2)}\n`,
  );
  process.exit(0);
}

if (verifySnapshotMode) {
  const baseline = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
  const observed = observeState(baseline);
  if (JSON.stringify(observed) !== JSON.stringify(baseline)) {
    throw new Error(
      "cache-only MCP runtime changed admitted cache/model bytes, entries, or mtimes",
    );
  }
  console.log(
    JSON.stringify({
      immutableStateUnchanged: true,
      roots: observed.roots.length,
      entries: observed.roots.reduce(
        (count, root) => count + root.entries.length,
        0,
      ),
    }),
  );
  process.exit(0);
}

async function connectClient(index) {
  let lastError;
  for (let attempt = 0; attempt < 240; attempt += 1) {
    const client = new Client({
      name: `rna-cache-only-${index}`,
      version: "1.0.0",
    });
    const transport = new StreamableHTTPClientTransport(new URL(endpoint));
    try {
      await client.connect(transport);
      return { client, transport };
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
  throw new Error(`MCP server did not become ready: ${lastError}`);
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

const before = baselinePath
  ? JSON.parse(fs.readFileSync(baselinePath, "utf8"))
  : snapshotState();
const clients = await Promise.all(
  Array.from({ length: clientCount }, (_, index) => connectClient(index)),
);

// One explicit warmup exercises the already-admitted resident runtime and is
// excluded from the latency sample.
const warmupMs = await timedSearch(clients[0].client);
const timings = [];
for (let round = 0; round < rounds; round += 1) {
  timings.push(
    ...(await Promise.all(clients.map(({ client }) => timedSearch(client)))),
  );
}

await Promise.all(clients.map(({ client }) => client.close()));
const after = observeState(before);
if (JSON.stringify(after) !== JSON.stringify(before)) {
  throw new Error(
    "cache-only MCP queries changed admitted cache/model bytes, entries, or mtimes",
  );
}

const sorted = [...timings].sort((left, right) => left - right);
const p95Ms = sorted[Math.ceil(sorted.length * 0.95) - 1];
const maxMs = sorted.at(-1);
if (p95Ms >= 2_000 || maxMs >= 10_000) {
  throw new Error(
    `cache-only latency exceeded contract: p95=${p95Ms.toFixed(1)}ms max=${maxMs.toFixed(1)}ms`,
  );
}

let phaseCounts;
if (serverLogPath) {
  const log = fs.readFileSync(serverLogPath, "utf8");
  const phases = {
    graph_load: 1,
    embedding_open: 1,
    encoder_asset_verification: 1,
    encoder_asset_post_verification: 1,
    strict_semantic_full_validation: 1,
    query_encoder_initialization: 1,
    reranker_initialization: 1,
    strict_reranker_full_validation: 1,
    query_encoder_wait: timings.length + 2,
    query_encoding: timings.length + 1,
    strict_semantic_resident_reuse: timings.length + 1,
    strict_reranker_resident_reuse: timings.length + 1,
    reranker_wait: timings.length + 1,
    candidate_retrieval: timings.length + 1,
    reranker_inference: timings.length + 1,
    root_discovery: timings.length + 1,
    enrichment_ledger_access: 0,
  };
  phaseCounts = Object.fromEntries(
    Object.entries(phases).map(([phase, expected]) => {
      const observed = log.split(`phase="${phase}"`).length - 1;
      if (observed !== expected) {
        throw new Error(
          `unexpected ${phase} count: expected=${expected} observed=${observed}`,
        );
      }
      return [phase, observed];
    }),
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
      cacheEntries: before.roots.find(({ label }) => label === "cache").entries
        .length,
      immutableRoots: before.roots.length,
      cacheUnchanged: true,
      immutableStateUnchanged: true,
      phaseCounts,
    },
    null,
    2,
  ),
);

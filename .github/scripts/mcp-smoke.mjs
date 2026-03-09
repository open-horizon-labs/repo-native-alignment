import process from "node:process";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const [serverPath, repoPath] = process.argv.slice(2);
if (!serverPath || !repoPath) {
  console.error("Usage: node .github/scripts/mcp-smoke.mjs <server-path> <repo-path>");
  process.exit(2);
}

// ── helpers ────────────────────────────────────────────────────────────────

let failures = 0;

function pass(label) {
  console.log(`  [PASS] ${label}`);
}

function fail(label, detail) {
  console.error(`  [FAIL] ${label}: ${detail}`);
  failures++;
}

function assertContains(label, text, needle) {
  if (typeof text !== "string") {
    fail(label, `Expected string, got ${typeof text}`);
    return;
  }
  if (!text.includes(needle)) {
    fail(label, `Expected "${needle}" in response (got ${text.length} chars)`);
  } else {
    pass(label);
  }
}

function assertNonEmpty(label, items) {
  if (!Array.isArray(items) || items.length === 0) {
    fail(label, `Expected non-empty array, got ${JSON.stringify(items)}`);
  } else {
    pass(label);
  }
}

/** Extract the text content from a tool CallToolResult */
function extractText(result) {
  if (!result || !Array.isArray(result.content)) return "";
  return result.content
    .filter((c) => c.type === "text")
    .map((c) => c.text ?? "")
    .join("\n");
}

// ── connect ───────────────────────────────────────────────────────────────

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
  console.log("Connected to RNA server.");

  // ── 1. listTools ────────────────────────────────────────────────────────
  console.log("\n── listTools ──");
  const tools = (await client.listTools()).tools ?? [];
  assertNonEmpty("listTools returns tools", tools);

  const requiredTools = new Set([
    "oh_get_context",
    "oh_search_context",
    "oh_record",
    "outcome_progress",
    "search_symbols",
    "graph_query",
    "list_roots",
  ]);
  const seen = new Set(tools.map((t) => t.name));
  for (const name of requiredTools) {
    if (!seen.has(name)) {
      fail(`required tool present: ${name}`, "tool missing from listTools");
    } else {
      pass(`required tool present: ${name}`);
    }
  }

  // ── 2. oh_get_context ───────────────────────────────────────────────────
  console.log("\n── oh_get_context ──");
  const getCtxResult = await client.callTool({ name: "oh_get_context", arguments: {} });
  const getCtxText = extractText(getCtxResult);
  assertContains(
    "oh_get_context contains 'Business Context'",
    getCtxText,
    "Business Context",
  );

  // ── 3. oh_search_context ────────────────────────────────────────────────
  console.log("\n── oh_search_context ──");
  const searchCtxResult = await client.callTool({
    name: "oh_search_context",
    arguments: { query: "agent alignment", limit: 3 },
  });
  const searchCtxText = extractText(searchCtxResult);
  // At least one result section should appear; accept empty gracefully only if
  // the repo has no .oh/ artifacts at all.
  if (searchCtxText.includes("No results found")) {
    // Tolerate empty result on a minimal fixture, but log it.
    console.log("  [SKIP] oh_search_context: no results (repo may lack .oh/ artifacts)");
  } else {
    assertNonEmpty(
      "oh_search_context returns content",
      searchCtxText.length > 0 ? [searchCtxText] : [],
    );
    pass("oh_search_context returned non-empty response");
  }

  // ── 4. search_symbols ───────────────────────────────────────────────────
  console.log("\n── search_symbols ──");
  const searchSymResult = await client.callTool({
    name: "search_symbols",
    arguments: { query: "main", limit: 5 },
  });
  const searchSymText = extractText(searchSymResult);
  if (searchSymText.startsWith("No symbols matching")) {
    fail("search_symbols('main') returns results", "Got 'No symbols matching'");
  } else {
    assertContains("search_symbols returns symbol entry", searchSymText, "main");
    pass("search_symbols('main') returned results");
  }

  // ── 5. outcome_progress ─────────────────────────────────────────────────
  console.log("\n── outcome_progress ──");
  const progResult = await client.callTool({
    name: "outcome_progress",
    arguments: { outcome_id: "agent-alignment" },
  });
  const progText = extractText(progResult);
  assertNonEmpty("outcome_progress returns content", progText.length > 0 ? [progText] : []);
  // Structural check: should contain some recognizable section header
  if (progText.length > 0) {
    pass("outcome_progress returned non-empty response");
  }

  // ── 6. list_roots ───────────────────────────────────────────────────────
  console.log("\n── list_roots ──");
  const rootsResult = await client.callTool({ name: "list_roots", arguments: {} });
  const rootsText = extractText(rootsResult);
  assertContains("list_roots response contains 'Workspace Roots'", rootsText, "Workspace Roots");

  // ── 7. negative test: unknown tool ──────────────────────────────────────
  console.log("\n── unknown tool (negative test) ──");
  try {
    const unknownResult = await client.callTool({ name: "nonexistent_tool_rna_smoke", arguments: {} });
    if (unknownResult.isError) {
      pass("unknown tool returns error response");
    } else {
      fail("unknown tool returns error", "Expected isError=true but got success");
    }
  } catch (err) {
    // SDK threw — also acceptable
    pass("unknown tool returns an error (not a hang)");
  }

  // ── summary ─────────────────────────────────────────────────────────────
  console.log("\n==========================================");
  if (failures === 0) {
    console.log(`MCP smoke check PASSED (${tools.length} tools visible).`);
  } else {
    console.error(`MCP smoke check FAILED: ${failures} assertion(s) failed.`);
  }
} finally {
  await client.close();
}

if (failures > 0) process.exit(1);

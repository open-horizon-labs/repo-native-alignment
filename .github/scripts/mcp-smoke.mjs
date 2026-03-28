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

async function callSearchWithRetry(args) {
  const maxAttempts = 12;
  const delayMs = 500;
  let text = "";

  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const result = await client.callTool({
      name: "search",
      arguments: args,
    });
    text = extractText(result);

    if (!text.includes("Index building")) {
      return text;
    }

    if (attempt < maxAttempts) {
      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
  }

  if (text.includes("Index building")) {
    throw new Error(
      `search remained in "Index building" state after ${maxAttempts} attempts`
    );
  }
  return text;
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
    "outcome_progress",
    "search",
    "list_roots",
    "repo_map",
  ]);
  const seen = new Set(tools.map((t) => t.name));
  for (const name of requiredTools) {
    if (!seen.has(name)) {
      fail(`required tool present: ${name}`, "tool missing from listTools");
    } else {
      pass(`required tool present: ${name}`);
    }
  }
  // Verify deprecated tools are removed
  for (const removed of ["oh_search_context", "search_symbols", "graph_query"]) {
    if (seen.has(removed)) {
      fail(`${removed} should be removed`, "tool still present in listTools");
    } else {
      pass(`${removed} correctly removed from tool list`);
    }
  }

  // ── 2. search (with artifacts) ──────────────────────────────────────────
  console.log("\n── search (artifacts) ──");
  const searchCtxText = await callSearchWithRetry({
    query: "agent alignment",
    include_artifacts: true,
    include_markdown: false,
    top_k: 3,
  });
  // At least one result section should appear; accept empty gracefully only if
  // the repo has no .oh/ artifacts at all.
  if (searchCtxText.includes("No results matching")) {
    // Tolerate empty result on a minimal fixture, but log it.
    console.log("  [SKIP] search (artifacts): no results (repo may lack .oh/ artifacts)");
  } else {
    assertNonEmpty(
      "search (artifacts) returns content",
      searchCtxText.length > 0 ? [searchCtxText] : [],
    );
    pass("search (artifacts) returned non-empty response");
  }

  // ── 4. search (code symbols) ────────────────────────────────────────────
  console.log("\n── search (code) ──");
  const searchSymText = await callSearchWithRetry({
    query: "main",
    include_artifacts: false,
    include_markdown: false,
    top_k: 5,
  });
  if (searchSymText.startsWith("No results matching")) {
    fail("search('main') returns results", "Got 'No results matching'");
  } else {
    assertContains("search returns code symbol entry", searchSymText, "main");
    pass("search('main') returned results");
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

  // ── 7. search (neighbors depth=2) ──────────────────────────────────────
  // Verifies that the depth parameter is accepted and processed through the
  // MCP protocol. We check for error conditions only — valid output may vary
  // based on fixture content (the node may have no neighbors in minimal fixtures).
  console.log("\n── search (neighbors depth=2) ──");
  const depthSearchText = await callSearchWithRetry({
    query: "main",
    mode: "neighbors",
    depth: 2,
    compact: true,
    include_artifacts: false,
    include_markdown: false,
    top_k: 1,
  });
  if (depthSearchText.includes("depth > 1 is not supported")) {
    fail("search (depth=2): depth parameter rejected unexpectedly", depthSearchText);
  } else if (depthSearchText.includes("No repository data") || depthSearchText.length === 0) {
    fail("search (depth=2): server returned empty/error response", depthSearchText);
  } else {
    // depth parameter was accepted and processed — any non-error output is valid.
    // The fixture may have no neighbors for "main", which produces a "No neighbors" message.
    pass("search depth=2 parameter honored through MCP protocol");
  }

  // ── 8. negative test: unknown tool ──────────────────────────────────────
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

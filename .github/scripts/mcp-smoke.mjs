import process from "node:process";
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const [serverPath, repoPath] = process.argv.slice(2);
if (!serverPath || !repoPath) {
  console.error("Usage: node .github/scripts/mcp-smoke.mjs <server-path> <repo-path>");
  process.exit(2);
}

const workItemLedgerPath = path.join(
  repoPath,
  ".oh",
  ".cache",
  "lsp_pass1_work_items.json",
);
const previousWorkItemLedger = fs.existsSync(workItemLedgerPath)
  ? fs.readFileSync(workItemLedgerPath)
  : null;
const now = Date.now();
fs.mkdirSync(path.dirname(workItemLedgerPath), { recursive: true });
fs.writeFileSync(
  workItemLedgerPath,
  JSON.stringify({
    schema_version: 4,
    records: {
      "mcp-smoke:0": {
        schema_version: 4,
        job_id: "mcp-smoke",
        item_id: 0,
        repo: repoPath,
        root: "fixture",
        file: "src/main.rs",
        node_id: "fixture:src/main.rs:main:function",
        node_name: "main",
        node_kind: "function",
        requested_operations: ["textDocument/references"],
        state: "in_flight",
        attempt_count: 1,
        current_phase: "mcp_smoke_probe",
        created_at_ms: now,
        updated_at_ms: now,
        started_at_ms: now,
      },
    },
  }),
);

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

function assertNotContains(label, text, needle) {
  if (typeof text !== "string") {
    fail(label, `Expected string, got ${typeof text}`);
    return;
  }
  if (text.includes(needle)) {
    fail(label, `Did not expect "${needle}" in response`);
  } else {
    pass(label);
  }
}

function assertMatches(label, text, pattern) {
  if (typeof text !== "string") {
    fail(label, `Expected string, got ${typeof text}`);
    return;
  }
  if (!pattern.test(text)) {
    fail(label, `Expected ${pattern} in response (got ${text.length} chars)`);
  } else {
    pass(label);
  }
}

function assertEqual(label, actual, expected) {
  if (actual !== expected) {
    fail(label, `Expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
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

function sha256File(filePath) {
  return createHash("sha256").update(fs.readFileSync(filePath)).digest("hex");
}

function assertRenderedBudget(label, text, maxBytes) {
  const actualBytes = Buffer.byteLength(text, "utf8");
  if (actualBytes > maxBytes) {
    fail(label, `Rendered ${actualBytes} UTF-8 bytes, budget was ${maxBytes}`);
    return;
  }
  const accounting = text.match(
    /- total: bytes=(\d+) chars=(\d+) estimated_tokens=(\d+)/,
  );
  if (!accounting) {
    fail(label, "Missing final render accounting");
    return;
  }
  const accountedBytes = Number.parseInt(accounting[1], 10);
  if (accountedBytes !== actualBytes) {
    fail(
      label,
      `Footer accounts for ${accountedBytes} bytes, actual MCP text is ${actualBytes}`,
    );
    return;
  }
  pass(label);
}

function sourceBodyBytes(text) {
  const start = text.indexOf("\n## Source bodies\n");
  if (start === -1) return 0;
  const remainder = text.slice(start + "\n## Source bodies\n".length);
  const sectionEnds = [
    remainder.indexOf("\n## Relationships\n"),
    remainder.indexOf("\n## Render accounting\n"),
  ].filter((offset) => offset >= 0);
  const section = remainder.slice(
    0,
    sectionEnds.length > 0 ? Math.min(...sectionEnds) : remainder.length,
  );
  let bytes = 0;
  for (const match of section.matchAll(/^(`{3,})text\n([\s\S]*?)\1$/gm)) {
    bytes += Buffer.byteLength(match[2], "utf8");
  }
  return bytes;
}

function assertTaskRoleOrDegradation(text, role) {
  if (text.includes(`role: ${role}`)) {
    pass(`task context delivers role ${role}`);
    return;
  }
  const hasOmissionSurface = text.includes("## Omissions and degradation");
  const debugRole = role
    .split("_")
    .map((word) => word[0].toUpperCase() + word.slice(1))
    .join("");
  const roleNamed = [role, debugRole].some((name) =>
    text.toLowerCase().includes(name.toLowerCase()),
  );
  const truthfullyDegraded = /\b(?:degraded|unavailable|missing|omitted|not covered)\b/i.test(
    text,
  );
  if (hasOmissionSurface && roleNamed && truthfullyDegraded) {
    pass(`task context truthfully degrades role ${role}`);
  } else {
    fail(
      `task context delivers or degrades role ${role}`,
      "Role was absent without a role-specific capability/omission explanation",
    );
  }
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

  const searchTool = tools.find((tool) => tool.name === "search");
  const searchProperties = searchTool?.inputSchema?.properties ?? {};
  for (const property of [
    "projection",
    "body_policy",
    "max_output_bytes",
    "max_output_tokens",
    "max_body_bytes",
    "max_total_body_bytes",
    "context_mode",
    "context_roles",
    "context_facets",
    "proposal",
  ]) {
    if (Object.hasOwn(searchProperties, property)) {
      pass(`search MCP schema exposes ${property}`);
    } else {
      fail(`search MCP schema exposes ${property}`, "property missing from inputSchema");
    }
  }
  for (const contractTerm of ["agent", "evidence", "task", "graph-delta-beta"]) {
    assertContains(
      `search MCP description advertises ${contractTerm}`,
      searchTool?.description ?? "",
      contractTerm,
    );
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
    compact: false,
    verbose: true,
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
  assertContains(
    "MCP search delivers per-file LSP completeness readiness",
    searchSymText,
    "benchmark per-file LSP completeness",
  );

  // ── 4a. projected search contract ───────────────────────────────────────
  console.log("\n── search (agent/evidence projections) ──");
  const agentProjectionText = await callSearchWithRetry({
    query: "hello",
    body_policy: "signature_only",
    max_output_bytes: 8192,
    include_artifacts: false,
    include_markdown: false,
    top_k: 3,
  });
  assertContains("default action projection is agent", agentProjectionText, "- projection: agent");
  assertContains(
    "agent projection reports final render cost",
    agentProjectionText,
    "## Render accounting",
  );
  for (const auditOnly of [
    "evidence.candidate_rank",
    "evidence.content_hash",
    "evidence.score.",
    "evidence.provenance",
    "evidence.diagnostic.",
    "## Candidate audit",
  ]) {
    assertNotContains(`agent projection omits ${auditOnly}`, agentProjectionText, auditOnly);
  }

  const evidenceProjectionText = await callSearchWithRetry({
    query: "hello",
    projection: "evidence",
    body_policy: "signature_only",
    max_output_bytes: 16384,
    include_artifacts: false,
    include_markdown: false,
    top_k: 3,
  });
  assertContains(
    "explicit audit projection is evidence",
    evidenceProjectionText,
    "- projection: evidence",
  );
  assertMatches(
    "evidence projection exposes typed selection audit",
    evidenceProjectionText,
    /evidence\.(?:candidate_rank|content_hash|score\.|provenance|diagnostic\.)/,
  );
  assertContains(
    "evidence projection exposes bounded candidate dispositions",
    evidenceProjectionText,
    "## Candidate audit",
  );
  assertRenderedBudget("evidence projection accounts for final MCP bytes", evidenceProjectionText, 16384);

  // A one-byte body budget is intentionally binding for every source symbol in
  // the fixture. The final output budget is checked against the actual MCP text,
  // not a pre-render estimate.
  console.log("\n── search (render/body budgets) ──");
  const outputBudgetBytes = 8192;
  const bodyBudgetBytes = 1;
  const budgetedProjectionText = await callSearchWithRetry({
    query: "hello",
    projection: "agent",
    body_policy: "complete",
    max_output_bytes: outputBudgetBytes,
    max_body_bytes: bodyBudgetBytes,
    max_total_body_bytes: bodyBudgetBytes,
    include_artifacts: false,
    include_markdown: false,
    top_k: 3,
  });
  assertRenderedBudget(
    "MCP search honors final rendered output budget",
    budgetedProjectionText,
    outputBudgetBytes,
  );
  const emittedBodyBytes = sourceBodyBytes(budgetedProjectionText);
  if (emittedBodyBytes <= bodyBudgetBytes) {
    pass("MCP search honors per-record and aggregate body budgets");
  } else {
    fail(
      "MCP search honors per-record and aggregate body budgets",
      `Rendered ${emittedBodyBytes} source-body bytes, budget was ${bodyBudgetBytes}`,
    );
  }
  assertNotContains(
    "binding body budget is not mislabeled complete",
    budgetedProjectionText,
    "- body: complete",
  );
  assertMatches(
    "binding body budget reports a typed omission",
    budgetedProjectionText,
    /- (?:per_record_body_cap|total_body_cap):/,
  );

  // ── 4b. role-aware task context ─────────────────────────────────────────
  console.log("\n── search (task context) ──");
  const taskContextText = await callSearchWithRetry({
    query: "Fix `hello` in lib.rs and update `test_hello`",
    projection: "agent",
    context_mode: "task",
    context_roles: ["editable_source", "test", "caller_or_impact"],
    context_facets: ["behavior", "test"],
    body_policy: "focused_span",
    max_output_bytes: 16384,
    include_artifacts: false,
    include_markdown: false,
  });
  assertContains(
    "task mode reports exact-reference capability",
    taskContextText,
    "task_exact_reference_resolution",
  );
  assertContains(
    "task mode reports bounded selection capability",
    taskContextText,
    "task_context_selection",
  );
  assertMatches("task mode emits a typed role", taskContextText, /\n   - role: [a-z_]+\n/);
  assertMatches("task mode emits a typed retrieval lane", taskContextText, /\n   - lane: [a-z_]+\n/);
  assertTaskRoleOrDegradation(taskContextText, "editable_source");
  assertTaskRoleOrDegradation(taskContextText, "test");
  assertTaskRoleOrDegradation(taskContextText, "caller_or_impact");
  assertRenderedBudget("task context honors the final output budget", taskContextText, 16384);

  // ── 4c. non-mutating graph-delta beta ───────────────────────────────────
  console.log("\n── search (graph-delta beta) ──");
  const proposalTarget = path.join(repoPath, "lib.rs");
  const proposalTargetBefore = sha256File(proposalTarget);
  const stableGraphQuery = {
    query: "hello",
    projection: "agent",
    body_policy: "none",
    max_output_bytes: 8192,
    include_artifacts: false,
    include_markdown: false,
    top_k: 1,
  };
  const graphViewBefore = await callSearchWithRetry(stableGraphQuery);
  const graphDeltaText = await callSearchWithRetry({
    query: "Review the proposed hello behavior change",
    projection: "evidence",
    context_mode: "graph-delta-beta",
    proposal: [
      "diff --git a/lib.rs b/lib.rs",
      "--- a/lib.rs",
      "+++ b/lib.rs",
      "@@ -2,1 +2,1 @@",
      '-pub fn hello() -> &\'static str { "world" }',
      '+pub fn hello() -> &\'static str { "rna" }',
    ].join("\n"),
    body_policy: "signature_only",
    max_output_bytes: 16384,
    include_artifacts: false,
    include_markdown: false,
  });
  assertContains("graph-delta returns the canonical card projection", graphDeltaText, "role: proposal_delta");
  assertContains("graph-delta returns the proposal lane", graphDeltaText, "lane: proposal_delta");
  assertContains("graph-delta returns capability state", graphDeltaText, "## Capability status");
  for (const capability of [
    "graph_delta_proposal_parsing",
    "graph_delta_live_graph_inference",
    "graph_delta_route_analysis",
    "graph_delta_card_coverage",
    "graph_delta_changed_files",
    "graph_delta_affected_locus_checklist",
    "proposal_overlay_persistence",
  ]) {
    assertMatches(
      `graph-delta reports ${capability} capability`,
      graphDeltaText,
      new RegExp(`${capability}: (?:ready|degraded|unavailable)`),
    );
  }
  assertContains(
    "graph-delta returns explicit omissions/degradation",
    graphDeltaText,
    "## Omissions and degradation",
  );
  assertRenderedBudget("graph-delta honors the final output budget", graphDeltaText, 16384);
  assertEqual(
    "graph-delta does not mutate its proposed source file",
    sha256File(proposalTarget),
    proposalTargetBefore,
  );
  const graphViewAfter = await callSearchWithRetry(stableGraphQuery);
  assertEqual(
    "graph-delta does not mutate the published graph view",
    createHash("sha256").update(graphViewAfter).digest("hex"),
    createHash("sha256").update(graphViewBefore).digest("hex"),
  );

  // ── 4d. persisted local-knowledge provenance ────────────────────────────
  console.log("\n── search (local-knowledge provenance) ──");
  const provenanceText = await callSearchWithRetry({
    query: "quote.mcp-provenance",
    compact: true,
    include_artifacts: false,
    include_markdown: false,
    top_k: 3,
  });
  assertContains(
    "MCP search exposes persisted Markdown provenance",
    provenanceText,
    "src:markdown",
  );
  assertContains(
    "MCP search exposes persisted local-knowledge metadata",
    provenanceText,
    "mcp_verified",
  );

  // ── 4b. search blank mode normalization ───────────────────────────────────
  console.log("\n── search (blank mode normalization) ──");
  const blankModeText = await callSearchWithRetry({
    query: "main",
    mode: "",
    include_artifacts: false,
    include_markdown: false,
    top_k: 3,
  });
  if (blankModeText.includes('Unknown mode: ""')) {
    fail('search mode="" behaves like omitted mode', blankModeText);
  } else {
    assertContains('search mode="" behaves like flat search', blankModeText, 'main');
  }

  // ── 4c. exact current-filesystem source span ─────────────────────────────
  console.log("\n── search (exact source span) ──");
  const diagnosticFixture = path.join(repoPath, "construction_error.rs");
  const diagnosticOutput = path.join(repoPath, ".oh", ".cache", "construction_error.rmeta");
  const compiler = spawnSync(
    "rustc",
    [diagnosticFixture, "--emit", "metadata", "-o", diagnosticOutput],
    { encoding: "utf8" },
  );
  const diagnosticMatch = (compiler.stderr ?? "").match(
    /--> .*construction_error\.rs:(\d+):(\d+)/,
  );
  if (!diagnosticMatch) {
    fail(
      "fixture compiler emits a source location",
      compiler.stderr || `rustc exited ${compiler.status}`,
    );
  }
  const diagnosticLocation = diagnosticMatch
    ? `construction_error.rs:${diagnosticMatch[1]}:${diagnosticMatch[2]}`
    : "construction_error.rs:2:17";
  const sourceSpanText = await callSearchWithRetry({
    file: diagnosticLocation,
  });
  assertContains(
    "MCP search retrieves construction site from fixture compiler diagnostic",
    sourceSpanText,
    "let value = MissingThing { field: 1 };",
  );
  assertContains(
    "MCP source span labels current filesystem provenance",
    sourceSpanText,
    "current filesystem state",
  );
  assertContains(
    "MCP source span reports root provenance",
    sourceSpanText,
    "**Root:**",
  );
  const explicitSpanText = await callSearchWithRetry({
    file: "lib.rs",
    line: 1,
    end_line: 1,
  });
  assertContains(
    "MCP search accepts explicit line and end_line arguments",
    explicitSpanText,
    '1 | pub fn main() { println!("{}", hello()); }',
  );

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
  assertContains(
    "list_roots delivers persisted LSP work queues through MCP",
    rootsText,
    "LSP Pass 1 Work Queues",
  );
  assertContains("list_roots delivers in-flight queue count", rootsText, "in_flight=1");
  assertContains(
    "list_roots delivers current LSP phase",
    rootsText,
    "mcp_smoke_probe=1",
  );

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
  fs.rmSync(path.join(repoPath, ".oh", ".cache", "construction_error.rmeta"), {
    force: true,
  });
  if (previousWorkItemLedger) {
    fs.writeFileSync(workItemLedgerPath, previousWorkItemLedger);
  } else {
    fs.rmSync(workItemLedgerPath, { force: true });
  }
}

if (failures > 0) process.exit(1);

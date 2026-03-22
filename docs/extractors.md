# Extractor Contribution Guide

RNA extracts symbols, imports, and topology edges from 22 languages via tree-sitter, then runs a set of post-extraction passes to enrich the graph with framework-aware edges (pub/sub boundaries, gRPC call chains, API links, etc.).

This document covers how to add new extraction capability — whether that is a framework-specific enrichment pass built into RNA, a repo-specific config file, or a new framework detection rule.

---

## Table of Contents

1. [Decision tree](#decision-tree)
2. [Approach 1: Built-in framework extractor (Rust)](#approach-1-built-in-framework-extractor-rust)
3. [Approach 2: Custom config extractor (.oh/extractors/*.toml)](#approach-2-custom-config-extractor-ohextractorstoml)
4. [Approach 3: Framework detection rules](#approach-3-framework-detection-rules)
5. [How to test an extractor](#how-to-test-an-extractor)
6. [Known limitations](#known-limitations)
7. [Language extractor reference](#language-extractor-reference)

---

## Decision tree

```
Is the framework used by many repos (not just yours)?
  Yes → Is the call pattern unambiguous at the import + body-text level?
    Yes → Approach 1: built-in Rust pass
    No  → Approach 2: config file (simpler, ships with the repo)
  No  → Is this an internal bus / proprietary broker / team wrapper?
    Yes → Approach 2: config file in .oh/extractors/
    No  → Add framework detection first (Approach 3), then pick 1 or 2

Do you just want RNA to recognise a framework from imports (no edges yet)?
  → Approach 3: add a row to FRAMEWORK_RULES in framework_detection.rs
```

**Rule of thumb:** start with Approach 2 (config file). Promote to Approach 1 (built-in Rust pass) only when the pattern is common enough to include in RNA itself and requires more logic than TOML can express (e.g., the KafkaJS `{topic: "..."}` object-literal extraction).

---

## Approach 1: Built-in framework extractor (Rust)

### When to use

- The framework is widely used (Kafka, gRPC, Celery, Socket.IO, etc.).
- The extraction pattern requires logic beyond substring matching (object-literal parsing, multi-hop indexing, cross-file join).
- You want the enrichment to work for all RNA users without any config files.

### Pattern overview

Every built-in enrichment step is a `PostExtractionPass` implementor registered in `PostExtractionRegistry::with_builtins()`. The registry runs all passes in order after tree-sitter extraction completes.

```
src/extract/
  your_framework.rs     ← new file: implements the pass logic
  post_extraction.rs    ← register YourFrameworkPass in with_builtins()
  framework_detection.rs ← add detection rule if framework not yet detected
```

### Step-by-step

**1. Create `src/extract/your_framework.rs`**

The file must export at least one public function (the pass body) and a `should_run` gate:

```rust
use std::collections::HashSet;
use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};

/// Return true when any detected framework requires this pass.
pub fn should_run(detected: &HashSet<String>) -> bool {
    detected.contains("your-framework-id")
}

/// Post-extraction pass: emit edges for YourFramework patterns.
pub fn your_framework_pass(all_nodes: &[Node]) -> Vec<Edge> {
    if !all_nodes.iter().any(|n| n.id.kind == NodeKind::Import
        && n.id.name.to_lowercase().contains("your_framework")) {
        return Vec::new();
    }

    let mut edges = Vec::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::Function || node.body.is_empty() {
            continue;
        }
        // ... pattern matching logic ...
        // edges.push(Edge { from: ..., to: ..., kind: EdgeKind::Produces, ... });
    }

    edges
}
```

Key conventions:
- `all_nodes` is the complete merged node list (all roots). Filter by `node.id.root != "external"` to skip LSP virtual nodes.
- Append new nodes/edges to the vecs passed in by the registry — do not replace them.
- Use `Confidence::Detected` for heuristic matches. Reserve `Confidence::Confirmed` for LSP-verified edges.
- Return `Vec<Edge>` (or an `ExtractionResult` if you also emit nodes).

**2. Add a pass struct in `post_extraction.rs`**

Open `src/extract/post_extraction.rs`. Add a new struct and its `PostExtractionPass` impl in the "Built-in pass implementations" section:

```rust
// --- YourFrameworkPass ---

struct YourFrameworkPass;

impl PostExtractionPass for YourFrameworkPass {
    fn name(&self) -> &str { "your_framework" }

    fn applies_when(&self, detected_frameworks: &HashSet<String>) -> bool {
        crate::extract::your_framework::should_run(detected_frameworks)
    }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, _ctx: &PassContext) -> PassResult {
        let new_edges = crate::extract::your_framework::your_framework_pass(nodes);
        if !new_edges.is_empty() {
            edges.extend(new_edges);
        }
        PassResult::empty()
    }
}
```

**3. Register in `with_builtins()`**

Still in `post_extraction.rs`, add your pass to `with_builtins()` in Group 3 (framework-gated passes — after `FrameworkDetectionPass`):

```rust
pub fn with_builtins() -> Self {
    let mut reg = Self::new();
    // Group 1: unconditional passes
    // ...
    // Group 2: framework detection — MUST run before any framework-gated pass
    reg.register(Box::new(FrameworkDetectionPass));
    // Group 3: framework-gated passes
    reg.register(Box::new(NextjsRoutingPass));
    reg.register(Box::new(PubSubPass));
    reg.register(Box::new(WebSocketPass));
    reg.register(Box::new(GrpcClientCallsPass));
    reg.register(Box::new(YourFrameworkPass));  // <-- add here
    // Group 4: config-driven passes
    reg.register(Box::new(ExtractorConfigPass));
    reg
}
```

**Critical ordering invariant:** framework-gated passes MUST be registered after `FrameworkDetectionPass`. A gated pass registered before framework detection sees an empty `detected_frameworks` set and silently skips every time.

**4. Add the module to `src/extract/mod.rs`**

```rust
pub mod your_framework;
```

### Real example: grpc.rs

`src/extract/grpc.rs` is the canonical example of a framework-gated built-in pass:

- `should_run()` gates on `grpc-python`, `grpc-go`, `grpc-js`, `grpc-java`.
- The pass builds an index of proto RPC method `Function` nodes (those with `parent_service` metadata), then scans caller function bodies for `stub.MethodName(` patterns.
- Emits `Calls` edges from callers to proto method nodes.
- Zero cost for repos without gRPC (framework gate + early-out on empty proto index).

### Real example: openapi_sdk_link.rs

`src/extract/openapi_sdk_link.rs` is an example of an unconditional pass (Group 1) that does a cross-node join:

- No `applies_when` gate — it is always attempted but exits cheaply when there are no generated SDK files.
- Joins `NodeKind::Function` nodes in generated SDK files (detected by filename patterns like `sdk.gen.ts`, `_generated.`, etc.) to `NodeKind::ApiEndpoint` nodes with matching `operation_id` metadata.
- Normalises camelCase/snake_case/PascalCase before matching so `listUsers`, `list_users`, `ListUsers` all resolve to the same endpoint.
- Emits `Implements` edges: `TS SDK function → ApiEndpoint → FastAPI handler`.

---

## Approach 2: Custom config extractor (.oh/extractors/*.toml)

### When to use

- Internal message bus, proprietary broker, or team-specific wrapper function.
- The pattern can be expressed as: "if file imports X and function body contains Y, emit a Produces/Consumes edge to topic Z (extracted from argument N)".
- No Rust changes needed — the config file ships with your repo.

### File location

```
<repo_root>/.oh/extractors/<name>.toml
```

Multiple files are supported — each file describes one extractor. Files that fail to parse are skipped with a warning; valid files in the same directory still load.

Only `.toml` files are loaded (`.md`, `.json`, `.yaml`, `README` are ignored).

### Format

```toml
[meta]
name = "my-extractor"
applies_when = { language = "python", imports_contain = "my.bus.client" }

[[boundaries]]
function_pattern = "bus.publish"
topic_arg = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "bus.subscribe"
topic_arg = 0
edge_kind = "Consumes"
```

#### `[meta]` fields

| Field | Required | Description |
|---|---|---|
| `name` | yes | Human-readable name, used in log messages |
| `applies_when.language` | yes | Node language string (e.g. `"python"`, `"typescript"`, `"go"`) |
| `applies_when.imports_contain` | yes | Substring that must appear in at least one `Import` node's body or name |

The `imports_contain` check is a substring test against both `node.id.name` and `node.body` of every `NodeKind::Import` node. Different language extractors store import text in different fields, so both are checked.

#### `[[boundaries]]` fields

| Field | Required | Description |
|---|---|---|
| `function_pattern` | yes | Substring or glob to match in function bodies (see below) |
| `topic_arg` / `arg_position` | no | 0-indexed argument position holding the topic name. **When omitted**, the matched function name itself is used as the channel name |
| `edge_kind` | yes | `"Produces"` or `"Consumes"` |
| `decorator` | no | Informational flag for decorator patterns; matching still uses body-text heuristic |

**`function_pattern` glob syntax:** `*` matches any sequence of identifier characters (`[a-zA-Z0-9_$.]`). This supports patterns like:

- `"publisher.publish"` — exact method call
- `"publish_*"` — any function starting with `publish_` (pub/sub wrapper convention)
- `"*.publish"` — `.publish` on any receiver object
- `"*publish*"` — any call containing `publish`

**Topic extraction:** RNA searches for a quoted string literal (single quote, double quote, or backtick) at the specified argument position. If the argument is a variable (not a literal), no edge is emitted (safe false negative). Template literals with `${...}` interpolation are also skipped.

### Examples

**Google Pub/Sub (Python)**

```toml
[meta]
name = "google-pubsub"
applies_when = { language = "python", imports_contain = "google.cloud.pubsub" }

[[boundaries]]
function_pattern = "publisher.publish"
topic_arg = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscriber.subscribe"
topic_arg = 0
edge_kind = "Consumes"
```

**Internal event bus with wrapper functions**

This pattern covers a team-specific wrapper where every function named `publish_*` emits an event and every `subscribe_*` function consumes one. The topic is extracted from the first argument.

```toml
[meta]
name = "internal-event-bus"
applies_when = { language = "python", imports_contain = "src.events.bus" }

[[boundaries]]
function_pattern = "publish_*"
topic_arg = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscribe_*"
topic_arg = 0
edge_kind = "Consumes"
```

**Function name as channel (no topic argument)**

When the function name IS the semantic boundary — for example, a queue consumer registered by decorator or a fixed-routing wrapper — omit `topic_arg`:

```toml
[meta]
name = "fixed-route-consumers"
applies_when = { language = "python", imports_contain = "src.workers" }

[[boundaries]]
function_pattern = "@worker.task"
edge_kind = "Consumes"
# topic_arg omitted: the function name becomes the channel name
```

**Multi-language bus (two config files)**

Create one file per language. RNA processes each root's `.oh/extractors/` independently.

`.oh/extractors/internal-bus-python.toml`:
```toml
[meta]
name = "internal-bus-python"
applies_when = { language = "python", imports_contain = "com.internal.bus" }

[[boundaries]]
function_pattern = "bus.send"
topic_arg = 0
edge_kind = "Produces"
```

`.oh/extractors/internal-bus-go.toml`:
```toml
[meta]
name = "internal-bus-go"
applies_when = { language = "go", imports_contain = "internal/bus" }

[[boundaries]]
function_pattern = "bus.Send"
topic_arg = 0
edge_kind = "Produces"
```

### How the pass runs

1. RNA scans `<repo_root>/.oh/extractors/*.toml` at each graph build (full or incremental).
2. For each config whose `imports_contain` string appears in any `Import` node, RNA scans all `Function`, `Impl`, and `Struct` nodes in the matching language.
3. For each body that contains `function_pattern(`, RNA extracts the topic from the specified argument position and emits a `Produces` or `Consumes` edge to a synthetic `NodeKind::Other("channel")` node.
4. Channel nodes are deduplicated: multiple functions publishing to the same topic share one channel node.
5. Edges from the same function to the same channel with the same direction are deduplicated.

**Per-root isolation:** Each workspace root's own `.oh/extractors/` configs are loaded independently. Imports from root A cannot activate configs from root B.

### Generating a config with /gen-extractor

If you have Claude Code available, describe the pattern in natural language and use the `/gen-extractor` skill:

```
/gen-extractor Our order service publishes to RabbitMQ using:
  channel.basic_publish(exchange="", routing_key="orders.created", body=data)
  and consumes with:
  channel.basic_consume(queue="payments.requests", on_message_callback=handle)
  The library is imported as "import pika"
```

The skill generates the `.oh/extractors/*.toml` file and drops it into your repo.

---

## Approach 3: Framework detection rules

### When to use

- You want RNA to recognise a new framework from import statements, even before writing any extraction pass.
- You are about to write a framework-gated pass (Approach 1) and need the framework ID to be in `detected_frameworks`.
- You just want `NodeKind::Other("framework")` nodes to show up for a framework so agents can query them.

### How detection works

`src/extract/framework_detection.rs` contains a static `FRAMEWORK_RULES` table. Each rule has:

- `import_pattern` — case-insensitive substring checked against `Import` node names.
- `language` — language string filter (empty = all languages).
- `framework_id` — stable ID written to graph nodes and returned in `detected_frameworks` (e.g. `"fastapi"`, `"kafkajs"`, `"grpc-go"`).
- `display_name` — human-readable name stored in node metadata.

The pass emits one `NodeKind::Other("framework")` node per detected framework, anchored at `frameworks/<framework-id>` in the primary root. It returns a `HashSet<String>` of all detected IDs which subsequent framework-gated passes check via `applies_when`.

### Adding a rule

Open `src/extract/framework_detection.rs` and add a `FrameworkRule` entry to `FRAMEWORK_RULES`. The table is grouped by language — add your rule in the appropriate section:

```rust
// In the Python section:
FrameworkRule {
    import_pattern: "my_framework",
    language: "python",
    framework_id: "my-framework",
    display_name: "My Framework",
},

// For TypeScript + JavaScript (add one entry per language):
FrameworkRule {
    import_pattern: "@my-org/my-framework",
    language: "typescript",
    framework_id: "my-framework",
    display_name: "My Framework",
},
FrameworkRule {
    import_pattern: "@my-org/my-framework",
    language: "javascript",
    framework_id: "my-framework",
    display_name: "My Framework",
},
```

**Specificity order matters.** More specific patterns must appear before more general ones. For example, `"next/"` rules for Next.js appear before the generic `"react"` rule so that Next.js imports don't clobber React detection.

**Language filter is mandatory for ambiguous patterns.** A pattern like `"kafka"` matches across languages — use a language filter to prevent Python's `kafka-python` rule from firing on Go import paths.

### What gets emitted

After adding a rule, RNA will:

1. Detect `my-framework` in `detected_frameworks` when any `Import` node matches.
2. Emit a `NodeKind::Other("framework")` node at `frameworks/my-framework`.
3. Make that node queryable: `search "" --kind framework`.
4. Allow subsequent passes to gate on `detected_frameworks.contains("my-framework")`.

---

## How to test an extractor

### Verify channel nodes were created

After writing a config extractor or built-in pass, scan the repo and search for channel nodes:

```bash
# Trigger a full scan (via the MCP tool or CLI)
scan --repo .

# List all channel nodes
search "" --kind other
```

Channel nodes appear as `NodeKind::Other("channel")` with the topic name. Framework nodes appear as `NodeKind::Other("framework")`.

### Verify edges were created

Use graph traversal to confirm edges from a known function to its channel:

```
graph_query --node "root:src/publisher.py:publish_order:Function" --mode neighbors
```

Expected output includes `Produces` edges to the channel node. The channel node's neighbours show `Consumes` edges inbound from subscriber functions.

### Unit test a built-in pass

Built-in passes should have unit tests in the same file. The pattern is:

1. Build a minimal node set (a few `Import` and `Function` nodes using the `make_import` / `make_fn` helpers).
2. Call the pass function directly with the node slice.
3. Assert on edge count, edge kind, and channel node name.

See `src/extract/extractor_config.rs` tests for the full pattern including adversarial cases (variable topics, wrong language, deduplication, template literal interpolation).

### Integration test a config extractor

The test suite has filesystem-based tests in `src/extract/post_extraction.rs` that write real `.oh/extractors/` files to `TempDir` and run `ExtractorConfigPass::run()`. These verify:

- Per-root isolation (configs from root A don't affect root B).
- Missing directory is a no-op.
- Malformed TOML is skipped without panicking.

To add a fixture config that is tested at the full-repo level, drop a `.toml` file in `tests/fixtures/oh_extractors/`. The existing `google-pubsub.toml` fixture is loaded by `test_load_extractor_configs_loads_google_pubsub_fixture`.

### Verify framework detection

```rust
#[test]
fn test_detects_my_framework() {
    let nodes = vec![make_import("repo", "python", "import my_framework")];
    let result = framework_detection_pass(&nodes, "repo");
    assert!(result.detected_frameworks.contains("my-framework"));
    assert!(result.nodes.iter().any(|n| n.id.name == "my-framework"));
}
```

---

## Known limitations

### Variable resolution requires 2-hop lookup

RNA extracts topic names from quoted string literals only. When the topic is stored in a variable:

```python
topic = get_topic_name()
publisher.publish(topic, data)
```

No edge is emitted. This is a deliberate safe false negative — inferring variable values requires dataflow analysis across multiple nodes, which RNA does not currently support. Document these cases and use a fixed-name boundary as a fallback if the variable always resolves to the same value.

### Router prefix requires a framework-specific extractor

Config extractors match against individual function bodies. They cannot reconstruct the full URL path when it is composed by concatenating a router prefix with a route suffix:

```python
router = APIRouter(prefix="/orders")

@router.get("/list")          # actual path: /orders/list
def list_orders(): ...
```

Resolving `/orders/list` from `prefix="/orders"` and `"/list"` requires a framework-specific extractor that walks the AST (like `nextjs_routing.rs` does for Next.js). For frameworks not yet supported, the emitted endpoint node will use the partial path.

### Argument extraction is shallow

The `topic_arg` parser advances past commas to find the N-th argument but does not handle:
- Nested function calls as arguments (`bus.publish(make_topic(), data)`)
- Multi-line argument lists with complex expressions
- Keyword arguments (Python `bus.publish(routing_key="orders")`)

For keyword-argument patterns, use a body-text substring that includes the keyword: `function_pattern = "routing_key=\"orders\""` with `topic_arg` omitted and a fixed channel name via a separate boundary entry.

### Config extractors do not gate on framework detection

Config extractors (`ExtractorConfigPass`) always run regardless of `detected_frameworks`. The gate is the `imports_contain` check at runtime. If you want a config extractor to skip for repos that don't have a specific framework detected, add a built-in pass (Approach 1) that calls `should_run()`.

### Glob patterns match identifier characters only

`*` in `function_pattern` expands to `[a-zA-Z0-9_$.]` — it does not match parentheses, brackets, or spaces. A pattern like `"bus.publish_*"` matches `bus.publish_orders(` but not `bus.publish orders(`. This is intentional: the glob is designed for method name completion, not arbitrary text.

---

## Language extractor reference

RNA includes 22 language extractors that run via tree-sitter to produce symbols, import graphs, and topology edges.

### Supported languages

- **Code** — Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, Bash, Ruby, C++, C#, Kotlin, Zig, Lua, Swift
- **Config & infra** — HCL/Terraform, JSON, TOML, YAML (Kubernetes manifests detected automatically)
- **Docs & schema** — Markdown (heading-aware), .proto, SQL, OpenAPI
- **Architecture** — subprocess, network, async boundaries detected as topology edges (Rust extractor)

### Constants and literals (cross-language)

All 22 extractors index constants and literal values. `search_symbols` returns the value inline:

```
- const MAX_RETRIES (rust) src/config.rs:12  Value: `5`
- const MAX_RETRIES (python) settings.py:3   Value: `5`
- const MAX_RETRIES (go) config.go:8         Value: `5`
```

Named constants are declared identifiers — `const MAX_RETRIES = 5`, static final fields, ALL_CAPS module-level assignments, etc.

Synthetic constants are inferred from structure — YAML/TOML/JSON top-level scalar values, OpenAPI enum values, and single-token string literals (e.g. `"application/json"`, `"GET"`) found in function bodies. They appear with a `*(literal)*` badge.

`search_symbols` accepts a `synthetic` filter to narrow results to declared constants, inferred literals, or both.

### Language-to-constant mapping

| Language | Extraction rule |
|---|---|
| Rust | `const_item` with extracted value |
| Python | Module-level ALL_CAPS assignments (`[A-Z][A-Z0-9_]+`) |
| TypeScript/JavaScript | Module-level `const` declarations |
| Go | `const_spec` inside `const_declaration` |
| Java | `static final` field declarations |
| Kotlin | `const val` property declarations |
| C# | `const` field declarations |
| Swift | Module-level `let` bindings |
| Zig | `const` variable declarations |
| C/C++ | `constexpr` and `static const` declarations |
| Lua/Ruby/Bash | ALL_CAPS module-level assignments |
| HCL | `variable` block default values |
| Proto | Enum values and `option` fields |
| SQL | `CREATE TYPE ... AS ENUM` values |
| YAML/TOML/JSON/OpenAPI | Top-level scalar values (synthetic) |

### Embeddings

Local Metal GPU via metal-candle (CPU fallback), no API key needed.

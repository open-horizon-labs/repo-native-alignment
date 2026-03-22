---
name: gen-extractor
description: Generate .oh/extractors/*.toml config files from natural language descriptions of framework/boundary patterns.
tools: Read, Write, Edit, Grep, Glob, Bash
mcpServers:
  - rna-mcp
---

# /gen-extractor

Generate `.oh/extractors/*.toml` config files that teach RNA to detect `Produces`/`Consumes` edges for message brokers, event buses, and other boundary patterns.

## Usage

```
/gen-extractor "detect Google Pub/Sub in Python — publisher.publish(topic_path) and subscriber.subscribe(subscription_path, callback)"
/gen-extractor "detect Redis pub/sub in Python — r.publish(channel, message) and r.subscribe(channel)"
/gen-extractor "detect RabbitMQ in Python — channel.basic_publish(exchange='', routing_key=topic, body=msg)"
```

**Proactive mode** — invoked without arguments: scans the repo for detected frameworks and flags gaps between what's detected and what's covered.

## Config format

Every `.oh/extractors/*.toml` file must have this structure:

```toml
[meta]
name = "my-extractor"
applies_when = { language = "python", imports_contain = "some.library" }

[[boundaries]]
function_pattern = "client.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "client.subscribe"
arg_position = 0
edge_kind = "Consumes"
```

**Fields:**

- `meta.name` — kebab-case identifier, matches the filename without `.toml`
- `meta.applies_when.language` — exact language string as RNA stores it (e.g., `"python"`, `"go"`, `"javascript"`, `"typescript"`, `"rust"`)
- `meta.applies_when.imports_contain` — substring that must appear in at least one `Import` node's body or name for this config to activate. Use the most specific stable prefix of the library path (e.g., `"google.cloud.pubsub"` matches `google.cloud.pubsub_v1`).
- `boundaries[].function_pattern` — substring of the call site to match, up to but NOT including `(`. RNA searches for `function_pattern + "("` so `"publisher.publish"` matches `publisher.publish(` but not `publisher.publish_to_dlq(`.
- `boundaries[].arg_position` — zero-indexed position of the argument that holds the topic/channel name as a quoted string literal. Use `0` for the first argument, `1` for the second, etc. Both `arg_position` and `topic_arg` are accepted (aliases).
- `boundaries[].edge_kind` — `"Produces"` (this code publishes/sends to the channel) or `"Consumes"` (this code subscribes/receives from the channel).
- `boundaries[].decorator` — optional boolean, set `true` for decorator-based patterns (e.g., `@bus.subscribe`). Currently informational only — matching still uses body-text heuristics.

**Important matching semantics:**
- RNA only extracts the topic when it is a **quoted string literal** (`"topic"`, `'topic'`, `` `topic` ``). Variables produce no edge (safe false negative).
- Template literals with interpolation (`` `topic-${env}` ``) are skipped.
- The import check is a **substring match** — `"google.cloud.pubsub"` matches `from google.cloud.pubsub_v1 import PublisherClient`.
- The function pattern match is **case-insensitive**.

## Process

### When given a description

**Step 1: Parse the description**

Extract from the user's description:
- The framework/library name (e.g., "Google Pub/Sub", "Redis", "RabbitMQ")
- The language (default to Python if unspecified)
- The call patterns for publishing (Produces) and subscribing (Consumes)
- The argument position of the topic/channel name in each call

**Step 2: Inspect the repo's imports**

Use the RNA CLI to find what import strings are actually present in the repo:

```bash
<rna-binary> search --repo <repo-path> "" --kind import --compact 2>&1 | grep -i "<framework-keyword>"
```

The `imports_contain` value must be a substring of one of these actual import strings. Pick the most specific stable prefix — avoid overly broad values like `"google"` (would match unrelated Google libraries).

If you can't find a matching import in the repo, note this. The config will still be valid — it simply won't fire until the library is imported.

**Step 3: Check for existing coverage**

```bash
ls <repo-path>/.oh/extractors/ 2>/dev/null
```

If a config already exists for this framework, show it and ask if the user wants to update it or create a variant.

**Step 4: Generate the config**

Write the TOML to `<repo-path>/.oh/extractors/<name>.toml`.

Name rules:
- Use kebab-case
- Name should describe the library, not the repo (e.g., `google-pubsub`, not `my-app-events`)
- Use the canonical library name where possible

**Step 5: Validate**

Re-scan the repo and verify edges appear:

```bash
<rna-binary> scan --repo <repo-path>
<rna-binary> search --repo <repo-path> "" --kind channel --compact 2>&1 | head -20
```

If channel nodes appear, the extractor is working. If not:

1. Check that the `imports_contain` value appears in actual import nodes (Step 2)
2. Check that the `function_pattern` appears in function bodies (search for it)
3. Check that the topic argument is a quoted string literal (not a variable)
4. Report what was found and what to try

**Step 6: Report**

Show the user:
- The generated config content
- How many channel nodes and Produces/Consumes edges were detected
- A sample of the detected channels (topic names found)
- Any functions that call the pattern but use variable topics (false negatives — this is expected behavior)

---

### Proactive mode (no arguments)

When invoked without arguments, scan for gaps between detected frameworks and existing extractor coverage.

**Step 1: Find detected frameworks**

```bash
<rna-binary> search --repo <repo-path> "" --kind framework --compact 2>&1
```

**Step 2: Find existing extractors**

```bash
ls <repo-path>/.oh/extractors/ 2>/dev/null
```

**Step 3: Find existing channel nodes (already covered)**

```bash
<rna-binary> search --repo <repo-path> "" --kind channel --compact 2>&1
```

**Step 4: Identify gaps**

Frameworks that are:
- Detected (appear in `--kind framework` output)
- AND are messaging/event/queue libraries (not HTTP frameworks, ORMs, testing tools)
- AND have no existing extractor coverage (no `.toml` file for them)
- AND have no channel nodes already (not covered by another extractor)

Messaging libraries to flag: Pub/Sub systems (Google Pub/Sub, AWS SNS/SQS, Azure Service Bus), message queues (RabbitMQ, ActiveMQ), streaming platforms (Kafka, Kinesis), in-process event buses (EventEmitter, PyDispatcher, Celery tasks), Redis pub/sub.

**Step 5: Report and offer**

For each uncovered messaging framework found:

```
Framework detected: redis (python)
No extractor coverage found.
Common Redis pub/sub patterns:
  - r.publish(channel, message)  → Produces
  - r.subscribe(channel)         → Consumes (older sync API)
  - pubsub.subscribe(channel)    → Consumes (pubsub object)

Want me to generate .oh/extractors/redis-pubsub.toml? (describe your actual call patterns if different)
```

If the user confirms, generate and validate the config (Steps 4-6 from the description-driven flow).

---

## Examples

### Example 1: Google Pub/Sub (Python)

Input:
```
/gen-extractor "detect Google Pub/Sub in Python — publisher.publish(topic_path, data) and subscriber.subscribe(subscription_path, callback)"
```

Generated `.oh/extractors/google-pubsub.toml`:
```toml
[meta]
name = "google-pubsub"
applies_when = { language = "python", imports_contain = "google.cloud.pubsub" }

[[boundaries]]
function_pattern = "publisher.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscriber.subscribe"
arg_position = 0
edge_kind = "Consumes"
```

### Example 2: Redis pub/sub (Python)

Input:
```
/gen-extractor "detect Redis pub/sub in Python — r.publish(channel, message) produces, pubsub.subscribe(channel) consumes"
```

Generated `.oh/extractors/redis-pubsub.toml`:
```toml
[meta]
name = "redis-pubsub"
applies_when = { language = "python", imports_contain = "redis" }

[[boundaries]]
function_pattern = "r.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "pubsub.subscribe"
arg_position = 0
edge_kind = "Consumes"
```

### Example 3: Internal event bus (Python)

Input:
```
/gen-extractor "detect our internal event bus — bus.publish('EventName', payload) produces, @bus.subscribe('EventName') consumes — imports src.events.bus"
```

Generated `.oh/extractors/internal-event-bus.toml`:
```toml
[meta]
name = "internal-event-bus"
applies_when = { language = "python", imports_contain = "src.events.bus" }

[[boundaries]]
function_pattern = "bus.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "@bus.subscribe"
arg_position = 0
edge_kind = "Consumes"
decorator = true
```

### Example 4: RabbitMQ (Python, topic in routing_key kwarg)

For patterns where the topic is a keyword argument (not positional), describe the wrapper function pattern that accepts it as a positional arg, or note this as a current limitation.

Current limitation: RNA extracts positional string literals only. If the topic is always a keyword argument (`routing_key="orders"`), use the function name as the pattern and note that topic extraction will fall back to the first positional quoted string. If no positional string is present, the edge will not be emitted.

---

## Locating the RNA binary

The RNA binary path depends on context:

- **In an RNA worktree:** `target/release/repo-native-alignment` (relative to repo root)
- **Installed globally:** `repo-native-alignment`
- **Debug build:** `target/debug/repo-native-alignment`

Check availability:
```bash
which repo-native-alignment 2>/dev/null || ls <repo-root>/target/release/repo-native-alignment 2>/dev/null || ls <repo-root>/target/debug/repo-native-alignment 2>/dev/null
```

Always pass `--repo <path-to-target-repo>` when running against a repo other than RNA itself.

---

## Testing against the Innovation-Connector reference repo

The canonical reference repo is `~/src/Innovation-Connector`. It has Google Pub/Sub imports and a `.oh/extractors/google-pubsub.toml` already present.

**Note:** Innovation-Connector uses topic paths stored in constants (`NEW_FILE_TOPIC = "files_event_v1_new_file"`), not inline string literals. The `publisher.publish` call pattern uses a variable (`topic_path`), not a literal. RNA extracts topics from quoted string literals only, so the reference config's `publish_new_file_message` boundaries produce no channel nodes in that repo — this is expected behavior (safe false negative for variable topics).

To test the skill itself:

1. **Check that imports are indexed** — the `from google.cloud.pubsub_v1 import SubscriberClient` import IS in the index. The `imports_contain = "google.cloud.pubsub"` condition fires correctly.

2. **Verify the config activates** — place a config with `function_pattern = "publisher.topic_path"` (which IS called with a constant string as the second arg):
   ```toml
   [[boundaries]]
   function_pattern = "publisher.topic_path"
   arg_position = 1
   edge_kind = "Produces"
   ```
   This would extract the topic name constant — but note the value is a Python variable name, not a literal. Still a variable reference, still no edge.

3. **To get actual channel nodes** in Innovation-Connector, you'd need a function that calls e.g.:
   ```python
   publisher.publish("projects/my-proj/topics/orders", data)
   ```
   directly with a literal. The reference config is intentionally written to match wrapper functions and documents this expectation with comments.

**For functional end-to-end testing**, use a repo with inline literal topics. The RNA test fixtures in `tests/fixtures/oh_extractors/` demonstrate the expected behavior with unit tests in `src/extract/extractor_config.rs`.

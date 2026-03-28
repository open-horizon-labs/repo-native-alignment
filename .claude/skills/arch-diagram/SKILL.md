# /arch-diagram

Generate an architecture diagram from RNA data. Teaches the traversal — the user picks the output format.

## When to Use

When the user asks for an architecture diagram, system overview, service map, or component diagram of a repo that has an RNA index.

## RNA Query Sequence

Execute these in order. Each step builds on the previous.

### Step 1: Workspace and Languages

```
list_roots(repo="<path>")
```

Extract: root names, languages detected, framework counts. This tells you the language mix and workspace boundaries.

### Step 2: Frameworks (Infrastructure Stack)

```
search(query="", kind="framework", repo="<path>", limit=30, compact=true)
```

Each framework node has a language context. Group them: "this is a C#/ASP.NET service with npgsql" vs "this is a TypeScript/React frontend." LLMs know what each framework does — use that knowledge to identify infrastructure dependencies (databases, caches, message brokers, external APIs).

### Step 3: API Surface

```
search(query="", kind="api_endpoint", repo="<path>", limit=30, compact=true)
```

API endpoints reveal the contract between services. Group by file path to identify which deployment unit owns which routes.

### Step 4: Entry Points and Hotspots

```
repo_map(repo="<path>")
```

Entry points reveal deployment units (each entry point = a runnable service/script). Hotspot files reveal where complexity concentrates. Active outcomes reveal what the system is for.

### Step 5: External Service Calls (Optional — for deeper diagrams)

```
search(query="https://api.", repo="<path>", limit=20)
search(query="connection", kind="const", repo="<path>", limit=10)
```

String literal constants containing API URLs and connection strings reveal external dependencies and can distinguish different uses of the same service (e.g., OpenAI completions endpoint vs embeddings endpoint).

## Inferring Architecture

### Deployment Units
Group by: entry points + directory prefixes + project files (`.csproj`, `package.json`, `Cargo.toml`). Each entry point from `repo_map` typically represents a separate deployable. Common patterns:
- `src/Services/X/` + entry point = a service
- `apps/web/` + React framework = SPA frontend
- `src/Workers/X/` + entry point = background worker
- `scripts/` = operational tooling (usually not a deployed service)

### Component Annotations
Every component should show:
- **Language** (from the nodes' language field)
- **Primary framework** (from framework detection)
- Example: "Northwoods.Api (C# / ASP.NET)" not just "API"

### Edges Between Components
- **Frontend → API**: inferred from framework combo (React + ASP.NET = SPA calling API)
- **Service → Database**: inferred from database frameworks (npgsql, mongoose, sqlalchemy)
- **Service → External API**: inferred from URL constants (Step 5)
- **Service → Object Store**: inferred from storage frameworks (minio, boto3, aws-sdk)

### Infrastructure Nodes
Databases, caches, message brokers, and external APIs are infrastructure — render them differently (cylinders for storage, clouds for external APIs). The framework names tell you what they are.

## Output

Produce the diagram in whatever format the user requests (d2, mermaid, plantuml, ASCII, etc.). If the user doesn't specify, ask.

The diagram should show:
1. **Deployment units** with language + framework labels
2. **Infrastructure** (databases, caches, queues, external APIs)
3. **Edges** with protocol/method annotations (HTTP, SQL, gRPC, etc.)
4. **Business context** if outcomes are available (what the system does, not just what it's built with)

Keep it high-level — this is architecture, not a class diagram. Collapse internal details. One box per deployable, not one box per file.

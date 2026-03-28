---
name: arch-diagram
description: Generate an architecture diagram from RNA data. Queries frameworks, endpoints, entry points, and external service calls, then renders in the user's preferred format (d2, mermaid, plantuml, etc.).
---

# /arch-diagram

Generate an architecture diagram from RNA data. Teaches the traversal — the user picks the output format.

## When to Use

When the user asks for an architecture diagram, system overview, service map, or component diagram of a repo that has an RNA index.

## RNA Query Sequence

Execute these in order. Each step builds on the previous.

### Step 1: Orientation (entry points, hotspots, outcomes)

```text
repo_map(repo="<path>")
```

Entry points reveal deployment units (each entry point = a runnable service/script). Hotspot files reveal where complexity concentrates. Active outcomes reveal what the system is for.

### Step 2: API Surface

```text
search(query="", kind="api_endpoint", repo="<path>", limit=30, compact=true)
```

API endpoints reveal the contract between services. Group by file path to identify which deployment unit owns which routes.

### Step 3: Frameworks (Infrastructure Stack)

```text
search(query="", kind="framework", repo="<path>", limit=30, compact=true)
```

Each framework node has a language context. Group them: "this is a C#/ASP.NET service with npgsql" vs "this is a TypeScript/React frontend." LLMs know what each framework does — use that knowledge to identify infrastructure dependencies (databases, caches, message brokers, external APIs).

### Step 4: Workspace and Languages

```text
list_roots(repo="<path>")
```

Extract: root names, languages detected, framework counts. This confirms the language mix and workspace boundaries.

### Step 5: Business Context

```text
outcome_progress(repo="<path>")
```

If outcomes are declared, this reveals what the system is trying to achieve — annotate the diagram with business purpose, not just technical structure.

### Step 6: External Service Calls (Optional — for deeper diagrams)

```text
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
- **Service → External API**: inferred from URL constants (Step 6)
- **Service → Object Store**: inferred from storage frameworks (minio, boto3, aws-sdk)

### Infrastructure Nodes
Databases, caches, message brokers, and external APIs are infrastructure — render them differently (cylinders for storage, clouds for external APIs). The framework names tell you what they are.

## Output

Produce the diagram in whatever format the user requests (d2, mermaid, plantuml, ASCII, etc.). If the user doesn't specify, ask.

The diagram should show:
1. **Deployment units** with language + framework labels
2. **Infrastructure** (databases, caches, queues, external APIs)
3. **Edges** with protocol/method annotations (HTTP, SQL, gRPC, etc.)
4. **Business context** from outcomes (what the system does, not just what it's built with)

Keep it high-level — this is architecture, not a class diagram. Collapse internal details. One box per deployable, not one box per file.

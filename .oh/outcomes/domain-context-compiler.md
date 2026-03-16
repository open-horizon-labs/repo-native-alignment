---
id: domain-context-compiler
status: proposed
---

# Domain Context Compiler

Agents build domain-specific graph+embed hybrid systems at runtime using RNA's existing pipeline. Any corpus where relationships dominate — infrastructure configs, legal docs, org structures, data schemas — gets the same treatment code gets today: entity extraction → relationship extraction → queryable graph + embeddings.

## The insight

RNA is a context compiler for code. The context compiler pattern generalizes:

```
domain artifacts → entity extraction → relationship extraction → graph + embeddings → structured retrieval → LLM reasoning
```

Today this pipeline is hardcoded to code (tree-sitter + LSP). The generalization: let agents define the extraction rules for their domain, and RNA runs the pipeline.

## Desired behavior change

An agent encountering a new domain in a repo (Kubernetes manifests, Terraform configs, OpenAPI specs, legal contracts, data pipeline DAGs) can:

1. Recognize structured artifacts it doesn't have a schema for
2. Author an extraction declaration in `.oh/extractors/`
3. Trigger a rescan
4. Query the domain graph with the same tools it uses for code

The agent builds its own domain model. No Rust code, no compilation, no waiting for a release.

## How it works

### Extraction declarations

`.oh/extractors/k8s-infra.toml`:

```toml
[meta]
name = "kubernetes-infrastructure"
description = "Services, databases, and dependencies from K8s manifests"
files = ["k8s/**/*.yaml", "helm/**/*.yaml", "deploy/**/*.yaml"]

[[entities]]
kind = "service"
match = { field = "kind", values = ["Deployment", "Service", "DaemonSet"] }
name = { field = "metadata.name" }
body = { field = "spec" }
metadata = { namespace = "metadata.namespace", image = "spec.template.spec.containers[0].image" }

[[entities]]
kind = "database"
match = { field = "kind", values = ["StatefulSet"] }
name = { field = "metadata.name" }
metadata = { storage = "spec.volumeClaimTemplates[0].spec.resources.requests.storage" }

[[relationships]]
kind = "depends_on"
detect = "env_ref"  # built-in: scan env vars for references to other entity names
from = { entity = "service" }
to = { entity = "*" }

[[relationships]]
kind = "exposes"
detect = "field_match"
from = { entity = "service" }
to = { entity = "service" }
field = "spec.selector.app"
target_field = "metadata.labels.app"
```

### What RNA does with this

1. **During scan**: match files against `files` globs. For each matching file, parse as YAML/TOML/JSON/CSV (format auto-detected or declared).
2. **Entity extraction**: apply `match` predicates. For each match, emit a `Node` with `kind = NodeKind::Other(entity.kind)`, `name` from the declared field, `body` from the declared field, and `metadata` from declared mappings.
3. **Relationship extraction**: apply relationship detectors. Built-in detectors:
   - `env_ref` — scan environment variables for references to other entity names
   - `field_match` — match field values between entities
   - `name_ref` — scan body text for references to other entity names
   - `jq` — arbitrary jq expression (for complex patterns)
4. **Graph + embed**: nodes and edges flow into the existing pipeline. Embedded alongside code symbols. Queryable via `search`, visible in `repo_map` subsystems.

### What this reuses from RNA

| RNA component | Reused for domain extraction |
|---|---|
| Scanner (mtime + BLAKE3) | Incremental detection of changed domain files |
| `Node` / `Edge` / `NodeKind::Other` | Domain entities and relationships |
| LanceDB persistence | Domain graph survives restart |
| Embedding pipeline | Semantic search over domain entities |
| `search` tool | `search(kind="service", mode="impact")` |
| `repo_map` + subsystem detection | Domain subsystems visible in orientation |
| `outcome_progress` | Track which domain entities changed |
| PageRank importance | Find architecturally important domain entities |

### What's new

| Component | Purpose | Complexity |
|---|---|---|
| TOML declaration parser | Read `.oh/extractors/*.toml` | ~100 lines |
| Structured file parser | YAML/TOML/JSON field path extraction | ~200 lines (serde_yaml + jsonpath) |
| Relationship detectors | `env_ref`, `field_match`, `name_ref`, `jq` | ~150 lines per detector |
| ExtractorRegistry integration | Register domain extractors alongside code extractors | ~50 lines |

### Example: what agents see after extraction

```
> repo_map

## Subsystems (8 detected)

- **extract** (47 symbols, cohesion: 0.82)
  Interfaces: extract_scan_result(), ExtractorRegistry

- **k8s-infra** (12 entities, cohesion: 0.71)
  Interfaces: api-gateway (service), postgres (database)

> search(kind="service", mode="impact", node="api-gateway")

## impact of k8s-infra:api-gateway:service

- **auth-service** (service) — depends_on via env ref AUTH_SERVICE_URL
- **user-service** (service) — depends_on via env ref USER_API
- **postgres** (database) — connects_to via PGHOST

> search(query="what depends on the database", rerank=true)

- **postgres** (database) k8s/statefulset.yaml — 3 dependents
- **redis** (database) k8s/redis.yaml — 1 dependent
```

## Beyond code repos

The same mechanism works for any structured artifact corpus:

| Domain | Extraction source | Entity types | Relationship types |
|---|---|---|---|
| Kubernetes | YAML manifests | services, databases, configs | depends_on, exposes, mounts |
| Terraform | HCL files | resources, modules, variables | references, depends_on |
| OpenAPI | YAML/JSON specs | endpoints, schemas, parameters | uses_schema, returns |
| Data pipelines | Airflow DAGs, dbt models | tasks, models, sources | upstream, downstream |
| CI/CD | GitHub Actions, GitLab CI | jobs, workflows, artifacts | depends_on, triggers |
| Legal | Contract markdown | clauses, parties, obligations | references, amends, supersedes |

Each domain gets a `.oh/extractors/*.toml` file. The agent authors it. RNA runs it.

## Success signals

- An agent encountering Kubernetes YAMLs in a repo authors `.oh/extractors/k8s.toml` without human help
- `search(kind="service")` returns domain entities alongside code symbols
- `repo_map` shows domain subsystems alongside code subsystems
- Domain entities have embeddings and are discoverable via semantic search
- Impact analysis crosses the code/domain boundary: "changing this function affects the api-gateway service config"

## Relationship to other outcomes

- **subsystem-detection** (#311): domain entities participate in subsystem clustering. A "k8s-infra" subsystem emerges naturally if domain entities are densely connected.
- **context-assembly**: domain extraction is a new source for the context compiler pipeline. Same graph, same embeddings, new entity types.
- **cross-API boundaries**: domain extractors are how cross-service edges get created. The K8s extractor knows `service A references service B` — that's a cross-API edge.

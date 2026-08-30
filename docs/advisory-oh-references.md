# Advisory Open Horizons references

RNA can retain stable references to authorized Open Horizons entities without
synchronizing either product's graph. This is soft referential integrity: it is
freshness-qualified and informative, never a condition for scanning, committing,
CI, or releasing a repository.

## Declaration contract

The existing local target form is unchanged:

```yaml
rna:
  kind: claim
  id: claim.local
  relationships:
    - kind: supports
      target:
        kind: claim
        id: claim.other
        file: .oh/knowledge/other.md
```

An Open Horizons target replaces `id`/`name`/`file` with `uri`:

```yaml
rna:
  kind: claim
  id: claim.local
  relationships:
    - kind: informs
      target:
        kind: endeavor
        uri: oh://v1/endeavor/endeavor%3Ashared%3A1
```

The URI must be byte-canonical `oh://v1/<kind>/<opaque-id>`. Supported kinds are
`context`, `endeavor`, `metis`, `guardrail`, `dive_pack`, and `log`. The URI,
not a mutable title, is the target identity, so renames do not change the local
edge. Archive and deletion are lifecycle results from the resolver.

Extraction creates only a local placeholder target. It performs no HTTP request
and does not infer that the entity exists. Frontmatter remains a detected
candidate under the content-source contract; it does not become body-confirmed
evidence merely because the external identity resolves.

## Resolution

Configure freshness in `.oh/config.toml`:

```toml
[open_horizons_references]
cache_ttl_seconds = 86400
```

The freshness window is hard-capped at 86,400 seconds (24 hours), including
CLI/MCP overrides. Repository configuration can shorten that window but cannot
extend it.

Put the exact resolver endpoint in `OPEN_HORIZONS_RESOLVER_URL` and an existing
`ak_` credential in `OPEN_HORIZONS_API_KEY`. A repository-controlled file is
never allowed to choose the destination that receives the credential. The CLI's
explicit `--resolver-url` is the only override. HTTP is rejected except for a
loopback test endpoint. RNA never writes either value to repository configuration
or its cache. The endpoint and API key must remain configured in offline mode:
they select the matching authority/credential cache namespace without making a
network request. A missing or different endpoint/key produces `unavailable`
rather than consuming another authority's cached response. Resolve all declared
targets, one explicit target, or use cache-only offline mode:

```bash
repo-native-alignment resolve-references --repo .
repo-native-alignment resolve-references \
  oh://v1/endeavor/endeavor%3Ashared%3A1 \
  --expected-kind endeavor --repo .
repo-native-alignment resolve-references --offline --repo .
```

The command is advisory and returns bounded JSON for every admitted declaration.
An unavailable,
unauthorized, unresolved, stale, or wrong-kind target does not make the command
or any ordinary RNA workflow fail.

Each explicit/discovered batch admits at most 256 declarations and spends at
most 12 seconds resolving them. Discovery visits at most 10,000 Markdown files,
reads at most 1 MiB per file and 16 MiB total, and emits at most 64 structured
advisory issues. Unreadable, non-UTF-8, and oversized files are skipped and
reported; they do not abort the remaining discovery. These bounds apply only to
this explicit advisory command/tool and do not alter scans, commits, CI, or
releases.

CLI and MCP explicit inputs share one preflight before URI parsing: only the
first 256 references are admitted, and a raw reference over 8 KiB is discarded
and replaced by a fixed nonsecret marker. Diagnostics contain bounded reason
codes, never the discarded value. Repository traversal also rejects file and
directory symlinks before following metadata, so a tracked symlink cannot make
discovery read Markdown outside the repository.

## Epistemic states

| State | Meaning |
|---|---|
| `confirmed` | The authorized resolver returned an active identity, or a cached authorized result is still within the configured freshness window. `source` distinguishes `network` from `cache`. |
| `retired` | The authorized entity exists but is archived or superseded. A fresh cached result remains explicitly cache-sourced. |
| `unresolved` | The URI is malformed/non-canonical or the public resolver returned its opaque not-found response. |
| `type_mismatch` | The declared/expected kind conflicts with the canonical URI kind. This is detected locally and sends no request. |
| `stale` | The last authorized minimal response exists but exceeds the freshness window. Lifecycle/version remain evidence as of `checked_at_unix_seconds`, never as current truth. |
| `unauthorized` | Authentication failed or the credential lacks access (`401`/`403`). Cached data for that identity is removed and is not used as a fallback. |
| `unavailable` | No usable cache exists and the endpoint, configuration, credential, transport, or response is unavailable. Network failure is not reported as a broken reference. |

On endpoint failure, a cached result may be used. It is `confirmed`/`retired`
only while inside the declared freshness window and `stale` after it. Offline
mode never contacts the endpoint and applies the same qualification.

## Redaction and data boundary

The request body contains exactly `reference` and, when supplied,
`expected_kind`. The response cache is outside the repository in the operating
system's user-owned cache directory. Its path is namespaced by SHA-256 digests
of the canonical repository path, normalized resolver endpoint, and
high-entropy API key. The key itself is never stored. Each cache contains only:

- canonical reference URI;
- entity kind;
- lifecycle (`active` or `retired`);
- opaque monotonic version;
- local check timestamp.

Repository-local and Git-tracked/preseeded cache-looking files are never read.
The user cache is bounded to 10,000 entries (oldest check time, then canonical
URI, is evicted first), atomically replaced, and restricted to the current owner
with owner-only permissions on Unix. Symlinks, non-regular files, wrong owners,
and group/world-readable files are rejected. A cross-process lock serializes
authorized load, network resolution, and mutation so a delayed success cannot
race a later authorization eviction. A `404` or authorization failure evicts
the entry. RNA never sends or caches repository nodes, edges, source/body text,
filenames, embeddings, search queries, outbox events, owners, titles, or the API
key. There is no cloud graph, traversal endpoint, push loop, or automatic
remediation in this feature.

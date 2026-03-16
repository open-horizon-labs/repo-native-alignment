# Evaluation Rubric

You are evaluating two design documents for a Spotify Connect adapter.
You do NOT know which agent produced which document. Score each independently.

## Scoring (1-5 per criterion)

### 1. Trait identification (20%)
- 5: Names `Startable` trait, `impl_startable!` macro, `AdapterHandle`, `SharedBus`, shows method signatures
- 3: Names `Startable` but misses macro or handle wrapper
- 1: Vague "implement a trait" without naming it

### 2. Bus integration (20%)
- 5: Correct `PrefixedZoneId::spotify(raw_id)` format, names specific `BusEvent` variants to emit (ZoneUpdate, NowPlaying, VolumeChanged), shows how aggregator consumes them
- 3: Mentions bus events but wrong format or missing zone ID prefix
- 1: Vague "send events to the bus"

### 3. Discovery approach (15%)
- 5: References `mdns.rs` for mDNS registration, references LMS UDP discovery as alternative pattern, describes Zeroconf service type for Spotify Connect (`_spotify-connect._tcp`)
- 3: Mentions mDNS but doesn't reference existing code
- 1: Doesn't address discovery

### 4. Reference adapter (15%)
- 5: Identifies the closest architectural match with specific reasoning (e.g., "OpenHome because it handles network discovery + transport control" or "LMS because of the TCP protocol similarity"), names specific files and patterns to follow
- 3: Names an adapter but weak reasoning
- 1: No reference adapter identified

### 5. Volume flow trace (15%)
- 5: Complete chain with function names and files: web UI → API handler → bus publish → aggregator → adapter → device. At least 4 hops named with exact function/file references
- 3: Partial chain, 2-3 hops, some function names
- 1: Vague "volume goes through the bus"

### 6. Config completeness (10%)
- 5: Spotify client_id, client_secret, device_name, bitrate, auth callback URL, token storage path — with config file field names matching existing patterns
- 3: Some config fields but missing auth flow
- 1: No config discussion

### 7. Specificity (5%)
- 5: Exact file paths (`src/adapters/spotify.rs`), struct names (`SpotifyAdapter`), enum variants (`PrefixedZoneId::spotify()`), function signatures
- 3: Some specific names, some vague
- 1: All vague ("create an adapter file")

## Output format

```json
{
  "solution_a": {
    "trait_identification": N,
    "bus_integration": N,
    "discovery": N,
    "reference_adapter": N,
    "volume_flow": N,
    "config": N,
    "specificity": N,
    "weighted_score": N.N,
    "notes": "..."
  },
  "solution_b": { ... },
  "winner": "a" | "b" | "tie",
  "confidence": "high" | "medium" | "low",
  "rationale": "..."
}
```

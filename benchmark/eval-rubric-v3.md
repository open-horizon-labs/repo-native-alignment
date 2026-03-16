# Evaluation Rubric V3

You are evaluating two developer exploration reports side by side.
You do NOT know which tool the author used. Score each independently.

## Scoring (1-5 per criterion)

### Q1: Volume flow trace (25%)
- 5: Complete chain with 6+ functions named, files + lines, from UI click through bus to adapter to device. Identifies where step size is resolved.
- 3: Partial chain (3-5 hops), some functions named, gaps in the middle
- 1: Vague ("volume goes through the bus") or only 1-2 hops

### Q2: New adapter checklist (20%)
- 5: Names Startable trait, AdapterLogic trait, impl_startable! macro, AdapterHandle, PrefixedZoneId constructor, config struct, coordinator registration, knobs route — with method signatures
- 3: Names the traits but misses macro, handle, or config patterns
- 1: Generic "implement a trait" without specifics

### Q3: Test coverage map (20%)
- 5: Lists every public function with coverage status (tested/untested/indirect), identifies which integration tests exercise which functions, names untested high-risk functions
- 3: Lists some functions with coverage status, misses indirect coverage
- 1: "No tests found" or incomplete inventory

### Q4: Zone struct blast radius (20%)
- 5: Lists 10+ files/functions that construct or destructure Zone, categorized (adapters, aggregator, API, tests), with line numbers
- 3: Lists 5-9 usage sites, some categorization
- 1: Only finds direct imports, misses construction sites

### Q5: Complexity ranking (15%)
- 5: Ranks adapters by complexity with specific function-level metrics (cyclomatic complexity, line count), identifies top 3 complex functions with what they call
- 3: Names the most complex adapter but no function-level detail
- 1: Guesses based on file size or vague assessment

## Quantitative metrics (report alongside scores)

For each solution, also report:
- Total functions/files named (specificity count)
- Any factual errors (wrong file, wrong function name, nonexistent code)
- Depth of analysis (max hop count in any call chain traced)

## Output format

```json
{
  "solution_a": {
    "q1_volume_flow": N,
    "q2_adapter_checklist": N,
    "q3_test_coverage": N,
    "q4_zone_blast_radius": N,
    "q5_complexity_ranking": N,
    "weighted_score": N.N,
    "functions_named": N,
    "factual_errors": N,
    "max_hop_depth": N,
    "notes": "..."
  },
  "solution_b": { ... },
  "winner": "a" | "b" | "tie",
  "confidence": "high" | "medium" | "low",
  "rationale": "..."
}
```

## Evaluation instructions

1. Read both solutions completely before scoring
2. Score each criterion independently — don't let one strong answer inflate others
3. Factual errors count against the score even if the answer is detailed
4. Specificity matters: "src/adapters/openhome.rs:755 control()" beats "the control function"
5. Mark any claims you cannot verify as "unverified" in notes

# Issue #825 amended selector evidence

This directory publishes the compact, verifier-clean evidence needed to audit
the amended selector and its superseding no-treatment decision without retaining
large transcripts, cache archives, patches, or evaluator logs in Git.

The original xarray A/T receipts demonstrate symmetric administrative censoring
at the original 600-second wall limit. Neither was evaluator-authorized or
evaluated. The final xarray A/T receipts come from fresh never-resumed sessions
under the publicly frozen 1,200-second / $6 amendment. The final Django A/T
receipts are the unchanged pair retained under the original 600-second / $3
registration. This distinction is intentional.

All four terminal patches were officially evaluated exactly once with evaluator
output kept out-of-band. Both arms resolved 2/2 cases with zero regressions. T
used 4,367,033 provider tokens versus A's 9,394,654 and 1,032.247 seconds versus
1,342.596 seconds of combined pre-evaluator wall time. Those outcomes and
aggregates remain immutable and independently verified.

The historical `final/selection-result.json` selected T, but a final ledger
audit proved that xarray T violated its exact system policy. Allowed model Bash
attempt 27 invoked `pip3 download`, despite the explicit network prohibition;
allowed attempt 37 inspected the sibling xarray A checkout, despite the explicit
prohibition on evidence from another arm. The deterministic
`final/superseding-selection-correction.json` binds that historical selection
hash, the exact actor-ledger hash, both action indices, and both command hashes.
Under the registered prerequisite, xarray T is noncompliant and the authoritative
decision is therefore **`no_RNA_treatment`**, classified
**`treatment_noncompliance`**, because at least one T episode failed the
mandatory RNA-first manipulation contract.

The xarray T evaluator invocation occurred after the now-proven nonadherence and
should not have occurred under the registration. It is retained and verified as
an erroneous post-nonadherence diagnostic outcome, not used to select a
treatment. This remains an **amended development selector**, not an
unchanged-preregistration or confirmatory effect estimate.

Verify from the repository root:

```console
python3 benchmark/swebench-rna-first/issue825/verify_published_result.py
```

The verifier binds the original/amended registration and selection bytes,
original timeout predicates, session freshness, retained Django hashes, zero
pre-evaluation invocation receipts, four official evaluation receipts, the
evaluation batch, exact treatment-policy bytes, the frozen xarray T action
ledger, the superseding correction, and the independently recomputed
no-treatment decision.

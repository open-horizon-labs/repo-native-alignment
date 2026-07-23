# Issue #825 amended selector evidence

This directory publishes the compact, verifier-clean evidence needed to audit
the selected mandatory RNA-first treatment without retaining large transcripts,
cache archives, patches, or evaluator logs in Git.

The original xarray A/T receipts demonstrate symmetric administrative censoring
at the original 600-second wall limit. Neither was evaluator-authorized or
evaluated. The final xarray A/T receipts come from fresh never-resumed sessions
under the publicly frozen 1,200-second / $6 amendment. The final Django A/T
receipts are the unchanged pair retained under the original 600-second / $3
registration. This distinction is intentional.

All four terminal patches were officially evaluated exactly once with evaluator
output kept out-of-band. Both arms resolved 2/2 cases with zero regressions. T
used 4,367,033 provider tokens versus A's 9,394,654 and 1,032.247 seconds versus
1,342.596 seconds of combined pre-evaluator wall time. The registered rule
therefore selected T. This is an **amended development selector**, not an
unchanged-preregistration or confirmatory effect estimate.

Verify from the repository root:

```console
python3 benchmark/swebench-rna-first/issue825/verify_published_result.py
```

The verifier binds the original/amended registration and selection bytes,
original timeout predicates, session freshness, retained Django hashes, zero
pre-evaluation invocation receipts, four official evaluation receipts, the
evaluation batch, and the independently recomputed selected-T result.

# Issue #830 exact-artifact selector successor

Issue #830 carries forward the two cases and counterbalanced arm order selected
under issue #827. It does not rerandomize or claim a second fresh selection.

The original selector stopped before any model, provider, credential, or
official-evaluator activity because its registered RNA artifact could not make
the selected SymPy tree READY. Issue #829 corrected those general producer
defects. This successor:

- keeps the issue #827 runner, treatment, runtime, evaluator, and selection
  rule unchanged;
- binds the successful exact-CI issue #829 artifact and trust anchors;
- reuses the verifier-clean issue #829 SymPy combined cache without scanning;
- prepares only the Django cache under the new producer identity; and
- preserves the original SymPy A→T and Django T→A order.

The first issue #830 launch also stopped before any model process or episode
receipt. It exposed two pre-model harness/setup defects: a false secret-name
match on the tokenizer provenance digest and tracked Django documentation
symlinks that static preflight had not audited. The successor closure retains
that failure, binds the shared exact-name correction, requires gateway-Python
and private-tree validation during preflight, and records the exact-tree
`core.symlinks=false` Django checkout preparation. Cases, order, model,
treatment, budget, timeout, evaluator, and retry policy remain unchanged.

The second issue #830 launch likewise stopped before Claude. Its retained token
ledger records `model_invoked=false`, zero provider requests, and zero CLI
turns. The exact canonical environment enables `PYTHONSAFEPATH=1`, exposing
three materialized trusted-plane entrypoints that had relied on Python
implicitly adding their script directory. Those entrypoints now explicitly
prepend only their own resolved, digest-bound, private-tree-audited materialized
directory. Safe-path mode remains enabled; neither the checkout, current
working directory, nor an ambient `PYTHONPATH` becomes trusted.

`successor-lineage.json` is the human-readable lineage contract. The generated
`registration.json` embeds that object and remains executable by the issue #827
runner. `selection.json` is published in a later commit so its
`registration_commit` can bind the immutable registration commit.

No credential, model, provider, or evaluator may be accessed while assembling
or validating these files.

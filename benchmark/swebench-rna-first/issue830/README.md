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

`successor-lineage.json` is the human-readable lineage contract. The generated
`registration.json` embeds that object and remains executable by the issue #827
runner. `selection.json` is published in a later commit so its
`registration_commit` can bind the immutable registration commit.

No credential, model, provider, or evaluator may be accessed while assembling
or validating these files.

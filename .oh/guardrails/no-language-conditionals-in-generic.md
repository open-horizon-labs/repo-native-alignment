---
id: no-language-conditionals-in-generic
outcome: agent-alignment
severity: hard
statement: No language-specific conditionals in generic.rs. All per-language behavior must go through LangConfig fields. If generic.rs checks language_name, a config field is missing.
---

## Rationale

An agent wrote `config.language_name == "rust"` in generic.rs — gating DependsOn type edges to Rust only. The GenericExtractor's entire purpose is being language-agnostic via LangConfig. The `/dissent` skill failed to catch this, calling it "honest and explicit" when it was actually half-implemented.

The fix was adding three LangConfig fields (param_container_field, param_type_field, return_type_field) and populating them per language in configs.rs. This is the correct pattern — the same one used for node_kinds, scope_parent_kinds, and const_value_field.

## Detection

`grep 'language_name ==' src/extract/generic.rs` should return nothing. Any match means a config field is missing.

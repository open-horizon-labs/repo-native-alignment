# LanceDB 0.31 production dependency backport

LanceDB 0.31.0 declares `lance-testing = 8.0.0` as a normal dependency, but
uses it only inside its own `#[cfg(test)]` modules. This pulls test tooling into
RNA production builds, including a vulnerable `quick-xml 0.26.0` path.

Upstream [PR #3661](https://github.com/lancedb/lancedb/pull/3661), approved at
commit `8e877bd920973bea198e3c15529f3c5c7504b5e4`, fixes the manifest by moving
`lance-testing` to `[dev-dependencies]`. That PR now targets the incompatible
LanceDB 0.32 beta / Lance 9 line, so RNA cannot pin its head while remaining on
LanceDB 0.31 / Lance 8.

This empty, Apache-2.0-licensed crate is a Cargo-native backport of those
manifest semantics. Dependency crates are not compiled with their own test
configuration, so LanceDB's production build references no API from this
stub. The exact `8.0.0` version keeps Cargo's Lance family coherent.

Remove the `[patch.crates-io]` entry and this directory when RNA upgrades to a
LanceDB release that includes the upstream dependency correction.

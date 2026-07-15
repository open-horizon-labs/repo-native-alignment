use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const REQUIRED_CASES: &[&str] = &[
    "body-backed-control",
    "metadata-only",
    "sidecar-only",
    "missing-body-evidence",
    "broken-anchor",
    "orphan-quote",
    "chart-screenshot",
    "stale-verification",
    "public-worksheet-prose",
];

const REQUIRED_FAILURES: &[(&str, &str, &str)] = &[
    (
        "metadata-only",
        "invalid",
        "content.metadata_without_body_evidence",
    ),
    (
        "sidecar-only",
        "invalid",
        "content.sidecar_without_body_evidence",
    ),
    (
        "missing-body-evidence",
        "unresolved",
        "content.missing_body_evidence",
    ),
    ("broken-anchor", "unresolved", "content.unresolved_anchor"),
    ("orphan-quote", "unresolved", "content.orphan_quote"),
    ("chart-screenshot", "invalid", "content.visual_not_evidence"),
    ("stale-verification", "stale", "content.stale_verification"),
    (
        "public-worksheet-prose",
        "invalid",
        "content.public_vocabulary_leak",
    ),
];

#[test]
fn adversarial_content_source_contract_is_complete_and_source_backed() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/content_source_contract");
    let manifest_text = fs::read_to_string(root.join("cases.toml")).unwrap();
    let manifest: toml::Value = toml::from_str(&manifest_text).unwrap();
    assert_eq!(manifest["contract_version"].as_integer(), Some(1));

    let cases = manifest["case"].as_array().unwrap();
    let ids: BTreeSet<_> = cases
        .iter()
        .map(|case| case["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, REQUIRED_CASES.iter().copied().collect());

    for case in cases {
        let id = case["id"].as_str().unwrap();
        let source = case["source"].as_str().unwrap();
        let source_text = fs::read_to_string(root.join(source))
            .unwrap_or_else(|error| panic!("{id}: cannot read {source}: {error}"));
        assert!(!source_text.trim().is_empty(), "{id}: empty source fixture");

        if let Some(sidecar) = case.get("sidecar").and_then(toml::Value::as_str) {
            assert!(
                root.join(sidecar).is_file(),
                "{id}: missing sidecar {sidecar}"
            );
        }

        let status = case["expected_status"].as_str().unwrap();
        let edge_count = case["expected_edges"].as_integer().unwrap();
        let diagnostics = case["expected_diagnostics"].as_array().unwrap();
        if status == "valid" {
            assert!(
                diagnostics.is_empty(),
                "{id}: valid control has diagnostics"
            );
            assert!(edge_count > 0, "{id}: valid control emits no edge");
            let snippet = case["evidence_snippet"].as_str().unwrap();
            assert!(
                source_text.contains(snippet),
                "{id}: evidence is not in body"
            );
        } else {
            assert_eq!(edge_count, 0, "{id}: invalid evidence emits an edge");
            let (_, required_status, required_diagnostic) = REQUIRED_FAILURES
                .iter()
                .find(|(required_id, _, _)| *required_id == id)
                .unwrap_or_else(|| panic!("{id}: missing locked failure expectation"));
            assert_eq!(status, *required_status, "{id}: wrong failure status");
            assert_eq!(
                diagnostics.as_slice(),
                [toml::Value::String((*required_diagnostic).to_owned())],
                "{id}: wrong diagnostic"
            );
        }
    }
}

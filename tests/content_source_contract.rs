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

        for fragment in case["required_fragments"].as_array().unwrap() {
            let fragment = fragment.as_str().unwrap();
            assert!(
                source_text.contains(fragment),
                "{id}: source no longer represents trigger {fragment:?}"
            );
        }

        if let Some(sidecar) = case.get("sidecar").and_then(toml::Value::as_str) {
            let sidecar_text = fs::read_to_string(root.join(sidecar))
                .unwrap_or_else(|error| panic!("{id}: cannot read sidecar {sidecar}: {error}"));
            for fragment in case["sidecar_required_fragments"].as_array().unwrap() {
                let fragment = fragment.as_str().unwrap();
                assert!(
                    sidecar_text.contains(fragment),
                    "{id}: sidecar no longer represents trigger {fragment:?}"
                );
            }
        }

        if let Some(asset) = case.get("asset").and_then(toml::Value::as_str) {
            let asset_bytes = fs::read(root.join(asset))
                .unwrap_or_else(|error| panic!("{id}: cannot read asset {asset}: {error}"));
            assert!(
                asset_bytes.starts_with(b"P3\n"),
                "{id}: asset is not a PPM bitmap"
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
            assert_expected_selector(case, &source_text, snippet);
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

fn assert_expected_selector(case: &toml::Value, source_text: &str, snippet: &str) {
    let selector = &case["expected_selector"];
    let byte_start = selector["byte_start"].as_integer().unwrap() as usize;
    let byte_end = selector["byte_end"].as_integer().unwrap() as usize;
    assert_eq!(
        &source_text.as_bytes()[byte_start..byte_end],
        snippet.as_bytes()
    );
    assert_eq!(selector["line_start"].as_integer(), Some(3));
    assert_eq!(selector["line_end"].as_integer(), Some(3));
    assert_eq!(selector["confidence"].as_str(), Some("confirmed"));
    assert_eq!(selector["validation_status"].as_str(), Some("valid"));
    for field in [
        "file_path",
        "body_node_id",
        "extractor_id",
        "pack_id",
        "rule_id",
    ] {
        assert!(
            !selector[field].as_str().unwrap().is_empty(),
            "empty {field}"
        );
    }
    assert_eq!(
        selector["snippet_hash"].as_str().unwrap(),
        blake3::hash(snippet.as_bytes()).to_hex().as_str()
    );
}

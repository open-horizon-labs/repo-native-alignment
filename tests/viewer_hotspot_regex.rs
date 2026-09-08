//! Regression test for #890 review finding on `src/viewer.html`'s fallback
//! hotspot-node parser.
//!
//! `sizeBasis` picks `m[4]` (the score, "churn N x complexity M = SCORE")
//! when churn is known, else falls back to the complexity capture for the
//! "churn: not available" branch. That branch's complexity is captured by
//! the regex's *fifth* group (`m[5]`), not the third (`m[3]`, which belongs
//! to the churn-known branch and is `undefined` here) -- reading the wrong
//! group made `sizeBasis` `NaN` for every hotspot row on a non-git root.
//!
//! This test extracts the real regex and `sizeBasis` expression straight out
//! of `src/viewer.html` (rather than hardcoding a copy that could drift from
//! the file) and evaluates them with Node against both hotspot-line shapes,
//! so a regression back to `m[3]` fails this test.

use std::path::Path;
use std::process::Command;

fn extract_line_containing<'a>(source: &'a str, needle: &str) -> &'a str {
    source
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("expected a line containing `{needle}` in viewer.html"))
        .trim()
}

/// Pull the regex literal out of `const m = line.match(/.../);`.
fn extract_regex_literal(line: &str) -> &str {
    let start = line
        .find("/^")
        .expect("expected a `/^`-anchored regex literal");
    let end = line
        .rfind("/)")
        .expect("expected the regex literal to close with `/)`");
    &line[start..end + 1]
}

/// Pull the whole `const sizeBasis = ...;` statement.
fn extract_size_basis_statement(line: &str) -> &str {
    assert!(
        line.starts_with("const sizeBasis ="),
        "unexpected sizeBasis line shape: {line}"
    );
    line
}

#[test]
fn viewer_hotspot_size_basis_uses_correct_capture_group() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node not found on PATH -- skipping viewer.html regex regression test");
        return;
    }

    let viewer_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viewer.html");
    let source = std::fs::read_to_string(&viewer_path).expect("read src/viewer.html");

    let regex_line =
        extract_line_containing(&source, "const m = line.match(/^- `([^`]+)` -- churn");
    let regex_literal = extract_regex_literal(regex_line);

    let size_basis_line = extract_line_containing(&source, "const sizeBasis =");
    let size_basis_stmt = extract_size_basis_statement(size_basis_line);

    let script = format!(
        r#"
        const regex = {regex_literal};
        function sizeBasisFor(line) {{
          const m = line.match(regex);
          if (!m) return null;
          {size_basis_stmt}
          return sizeBasis;
        }}
        const known = sizeBasisFor("- `src/foo.rs` -- churn 5 x complexity 10 = 50");
        const unavailable = sizeBasisFor("- `src/foo.rs` -- churn: not available (no .git); complexity 7");
        console.log(JSON.stringify({{known, unavailable}}));
        "#
    );

    let output = Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .expect("failed to run node");
    assert!(
        output.status.success(),
        "node script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("failed to parse node output `{stdout}`: {e}"));

    // Churn-known branch: sizeBasis is the score (m[4]) -- unaffected by
    // this bug, but asserted here so a future edit can't silently change it
    // while "fixing" the unavailable branch.
    assert_eq!(
        parsed["known"],
        serde_json::json!(50),
        "known-churn branch: {parsed}"
    );

    // Churn-unavailable branch: sizeBasis must be the complexity value (7),
    // not NaN (which `JSON.stringify` renders as `null`).
    assert_eq!(
        parsed["unavailable"],
        serde_json::json!(7),
        "unavailable-churn branch should size by complexity (m[5]), not NaN: {parsed}"
    );
}

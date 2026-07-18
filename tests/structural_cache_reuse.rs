//! End-to-end proof for verifier-owned structural-cache reuse.
//!
//! This exercises the same binary/archive/injection seams used by the frozen
//! cohort qualifier. The tiny deterministic LSP servers keep the regression
//! offline while still producing a real cross-file Python LSP edge.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use repo_native_alignment::server::load_graph_from_lance;
use serde_json::Value;

const AUTHORIZATION_SHA_ENV: &str = "RNA_STRUCTURAL_CACHE_AUTHORIZATION_SHA256";

fn checked(mut command: Command, label: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label} failed to start: {error}"));
    assert!(
        output.status.success(),
        "{label} failed ({:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    String::from_utf8(checked(command, &format!("git {}", args.join(" "))).stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write fixture executable");
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn make_server_wrappers(root: &Path) -> (PathBuf, String) {
    let bin = root.join("fixture-bin");
    fs::create_dir_all(&bin).unwrap();
    let fixture = fixture_path("tests/fixtures/lsp_capability_server.py");
    let fixture = fixture.to_str().expect("fixture path is UTF-8");
    write_executable(
        &bin.join("pyright-langserver"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"--version\" ]; then echo 'fixture-pyright 1.0'; exit 0; fi\nexec python3 \"{fixture}\" python_features\n"
        ),
    );
    write_executable(
        &bin.join("rna-config-language-server"),
        &format!(
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"--version\" ]; then echo 'fixture-config 1.0'; exit 0; fi\nexec python3 \"{fixture}\" document_zero\n"
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("compose fixture PATH")
    .to_string_lossy()
    .into_owned();
    (bin, path)
}

#[derive(Debug)]
struct FixtureHistory {
    bare: PathBuf,
    base: String,
    changed: String,
    renamed: String,
    configured: String,
    deleted: String,
}

fn make_fixture_history(root: &Path) -> FixtureHistory {
    let source = root.join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("tests")).unwrap();
    git(&source, &["init", "--quiet"]);
    git(&source, &["config", "user.name", "RNA Fixture"]);
    git(
        &source,
        &["config", "user.email", "rna-fixture@example.invalid"],
    );
    fs::write(
        source.join("src/app.py"),
        "def greet(name: str) -> str:\n    message = f\"Hello, {name}\"\n    return message  # baseline\n",
    )
    .unwrap();
    fs::copy(
        fixture_path("tests/fixtures/lsp_capability_repo/tests/test_app.py"),
        source.join("tests/test_app.py"),
    )
    .unwrap();
    for index in 0..4 {
        fs::write(
            source.join(format!("src/unchanged_{index}.py")),
            format!("# unchanged partition member {index}\n"),
        )
        .unwrap();
    }
    fs::write(
        source.join("pyproject.toml"),
        "[project]\nname = \"rna-cache-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    git(&source, &["add", "--all"]);
    git(&source, &["commit", "--quiet", "-m", "base"]);
    let base = git(&source, &["rev-parse", "HEAD"]);

    fs::write(
        source.join("src/app.py"),
        "def greet(name: str) -> str:\n    message = f\"Hello, {name}\"\n    return message  # updated body only\n",
    )
    .unwrap();
    git(&source, &["add", "src/app.py"]);
    git(
        &source,
        &["commit", "--quiet", "-m", "body-only Python change"],
    );
    let changed = git(&source, &["rev-parse", "HEAD"]);

    git(
        &source,
        &["mv", "tests/test_app.py", "tests/test_renamed.py"],
    );
    git(&source, &["commit", "--quiet", "-m", "rename test"]);
    let renamed = git(&source, &["rev-parse", "HEAD"]);

    fs::write(
        source.join("pyproject.toml"),
        "[project]\nname = \"rna-cache-fixture\"\nversion = \"0.2.0\"\n",
    )
    .unwrap();
    git(&source, &["add", "pyproject.toml"]);
    git(
        &source,
        &["commit", "--quiet", "-m", "change Python configuration"],
    );
    let configured = git(&source, &["rev-parse", "HEAD"]);

    fs::remove_file(source.join("src/app.py")).unwrap();
    git(&source, &["add", "--all"]);
    git(&source, &["commit", "--quiet", "-m", "delete source"]);
    let deleted = git(&source, &["rev-parse", "HEAD"]);

    let bare = root.join("owner__repo.git");
    let mut clone = Command::new("git");
    clone
        .args(["clone", "--quiet", "--bare"])
        .arg(&source)
        .arg(&bare);
    checked(clone, "create bare fixture repository");
    FixtureHistory {
        bare,
        base,
        changed,
        renamed,
        configured,
        deleted,
    }
}

fn clone_at(root: &Path, name: &str, bare: &Path, commit: &str) -> PathBuf {
    let parent = root.join(name);
    let checkout = parent.join("checkout");
    fs::create_dir_all(&parent).unwrap();
    let mut clone = Command::new("git");
    clone
        .args(["clone", "--quiet", "--no-checkout"])
        .arg(bare)
        .arg(&checkout);
    checked(clone, "clone fixture checkout");
    git(&checkout, &["checkout", "--quiet", "--detach", commit]);
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    );
    checkout
}

fn json_file(path: &Path) -> Value {
    serde_json::from_slice(
        &fs::read(path).unwrap_or_else(|error| panic!("read JSON {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse JSON {}: {error}", path.display()))
}

fn run_driver(
    action: &str,
    rna: &Path,
    checkout: &Path,
    archive: &Path,
    sidecar: &Path,
    bare: Option<&Path>,
    fixture_path_env: &str,
) -> Value {
    let mut command = Command::new("python3");
    command
        .arg(fixture_path("tests/fixtures/structural_cache_driver.py"))
        .arg(action)
        .arg("--rna")
        .arg(rna)
        .arg("--checkout")
        .arg(checkout)
        .arg("--archive")
        .arg(archive)
        .arg("--sidecar")
        .arg(sidecar)
        .env("PATH", fixture_path_env);
    if let Some(bare) = bare {
        command.arg("--git-dir").arg(bare);
    }
    let output = checked(command, &format!("structural cache {action}"));
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse structural cache {action} output: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn scan_ready(
    rna: &Path,
    checkout: &Path,
    fixture_path_env: &str,
    reference_path: &str,
    authorization_sha256: Option<&str>,
) -> Value {
    let mut scan = Command::new(rna);
    scan.arg("--business-context")
        .arg("disabled")
        .arg("scan")
        .arg("--repo")
        .arg(checkout)
        .arg("--full")
        .arg("--no-embed")
        .arg("--timings")
        .current_dir(checkout)
        .env("PATH", fixture_path_env)
        .env("RNA_LSP_FIXTURE_PYTHON_REFERENCE_PATH", reference_path);
    if let Some(digest) = authorization_sha256 {
        scan.env(AUTHORIZATION_SHA_ENV, digest);
    }
    checked(scan, "RNA full structural scan");

    let mut readiness = Command::new(rna);
    readiness
        .arg("--business-context")
        .arg("disabled")
        .arg("lsp-readiness")
        .arg("--repo")
        .arg(checkout)
        .arg("--json")
        .current_dir(checkout)
        .env("PATH", fixture_path_env)
        .env("RNA_LSP_FIXTURE_PYTHON_REFERENCE_PATH", reference_path);
    if let Some(digest) = authorization_sha256 {
        readiness.env(AUTHORIZATION_SHA_ENV, digest);
    }
    let output = checked(readiness, "fresh RNA readiness validation");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse readiness output: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(value["ready"], true, "readiness output: {value:#}");
    assert_eq!(value["compatibility_violations"], serde_json::json!([]));
    assert_eq!(value["report"]["violations"], serde_json::json!([]));
    value["report"].clone()
}

fn injected_scan(
    rna: &Path,
    checkout: &Path,
    archive: &Path,
    sidecar: &Path,
    bare: &Path,
    fixture_path_env: &str,
    reference_path: &str,
) -> (Value, Value) {
    let injection = run_driver(
        "inject",
        rna,
        checkout,
        archive,
        sidecar,
        Some(bare),
        fixture_path_env,
    );
    let authorization_sha256 = injection["authorization_sha256"]
        .as_str()
        .expect("injection receipt authorization digest");
    let report = scan_ready(
        rna,
        checkout,
        fixture_path_env,
        reference_path,
        Some(authorization_sha256),
    );
    let execution = json_file(&checkout.join(".oh/.cache/structural-cache-execution.json"));
    (report, execution)
}

fn string_set(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("expected JSON array")
        .iter()
        .map(|item| item.as_str().expect("expected JSON string").to_string())
        .collect()
}

fn archive_case(
    rna: &Path,
    checkout: &Path,
    evidence: &Path,
    name: &str,
    fixture_path_env: &str,
) -> (PathBuf, PathBuf) {
    let archive = evidence.join(format!("{name}.tar.gz"));
    let sidecar = evidence.join(format!("{name}.manifest.json"));
    run_driver(
        "archive",
        rna,
        checkout,
        &archive,
        &sidecar,
        None,
        fixture_path_env,
    );
    (archive, sidecar)
}

async fn assert_fresh_graph_has_no_path(checkout: &Path, stale_path: &str) {
    let graph = load_graph_from_lance(checkout)
        .await
        .expect("fresh Lance graph reload succeeds");
    assert!(!graph.nodes.is_empty(), "fresh Lance graph is populated");
    assert!(
        graph
            .nodes
            .iter()
            .all(|node| node.id.file != Path::new(stale_path)),
        "fresh graph retained stale node path {stale_path}"
    );
    assert!(
        graph.edges.iter().all(|edge| {
            edge.from.file != Path::new(stale_path) && edge.to.file != Path::new(stale_path)
        }),
        "fresh graph retained stale edge endpoint {stale_path}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_cache_chain_reuses_then_incrementally_refreshes_real_persisted_lsp_graph() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let (_fixture_bin, fixture_path_env) = make_server_wrappers(root);
    let history = make_fixture_history(root);
    let evidence = root.join("evidence");
    fs::create_dir_all(&evidence).unwrap();
    let rna = PathBuf::from(env!("CARGO_BIN_EXE_repo-native-alignment"));

    let cold = clone_at(root, "cold", &history.bare, &history.base);
    let cold_report = scan_ready(&rna, &cold, &fixture_path_env, "tests/test_app.py", None);
    assert!(
        cold_report["readiness_validation_requests_by_language"]["python"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        cold_report["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|evidence| evidence["disposition"] == "executed")
    );
    let cold_graph = load_graph_from_lance(&cold).await.unwrap();
    assert!(cold_graph.edges.iter().any(|edge| {
        (edge.from.file == Path::new("src/app.py")
            && edge.to.file == Path::new("tests/test_app.py"))
            || (edge.to.file == Path::new("src/app.py")
                && edge.from.file == Path::new("tests/test_app.py"))
    }));
    let (base_archive, base_sidecar) =
        archive_case(&rna, &cold, &evidence, "base", &fixture_path_env);

    let identical = clone_at(root, "identical", &history.bare, &history.base);
    let (identical_report, identical_execution) = injected_scan(
        &rna,
        &identical,
        &base_archive,
        &base_sidecar,
        &history.bare,
        &fixture_path_env,
        "tests/test_app.py",
    );
    assert!(string_set(&identical_execution["executed_paths"]).is_empty());
    assert_eq!(
        identical_execution["executed_graph_enrichment_operation_count"],
        0
    );
    assert!(
        identical_execution["inherited_graph_enrichment_operation_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        identical_execution["inherited_readiness_validation_request_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        identical_execution["executed_readiness_validation_request_count"],
        0
    );
    assert!(
        identical_report["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|evidence| evidence["disposition"] == "verified_inherited")
    );
    assert_fresh_graph_has_no_path(&identical, "does/not/exist.py").await;
    let (identical_archive, identical_sidecar) =
        archive_case(&rna, &identical, &evidence, "identical", &fixture_path_env);

    let changed = clone_at(root, "changed", &history.bare, &history.changed);
    let (_changed_report, changed_execution) = injected_scan(
        &rna,
        &changed,
        &identical_archive,
        &identical_sidecar,
        &history.bare,
        &fixture_path_env,
        "tests/test_app.py",
    );
    assert_eq!(
        string_set(&changed_execution["executed_paths"]),
        BTreeSet::from(["src/app.py".to_string(), "tests/test_app.py".to_string()]),
        "body-only change should execute only its cross-file LSP closure"
    );
    assert!(
        changed_execution["executed_graph_enrichment_operation_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        changed_execution["inherited_graph_enrichment_operation_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        changed_execution["inherited_readiness_validation_request_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        changed_execution["executed_readiness_validation_request_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_fresh_graph_has_no_path(&changed, "does/not/exist.py").await;
    let (changed_archive, changed_sidecar) =
        archive_case(&rna, &changed, &evidence, "changed", &fixture_path_env);

    let renamed = clone_at(root, "renamed", &history.bare, &history.renamed);
    let (_renamed_report, renamed_execution) = injected_scan(
        &rna,
        &renamed,
        &changed_archive,
        &changed_sidecar,
        &history.bare,
        &fixture_path_env,
        "tests/test_renamed.py",
    );
    let renamed_paths = string_set(&renamed_execution["executed_paths"]);
    assert!(renamed_paths.contains("tests/test_app.py"));
    assert!(renamed_paths.contains("tests/test_renamed.py"));
    assert_fresh_graph_has_no_path(&renamed, "tests/test_app.py").await;
    let (renamed_archive, renamed_sidecar) =
        archive_case(&rna, &renamed, &evidence, "renamed", &fixture_path_env);

    let configured = clone_at(root, "configured", &history.bare, &history.configured);
    let (_configured_report, configured_execution) = injected_scan(
        &rna,
        &configured,
        &renamed_archive,
        &renamed_sidecar,
        &history.bare,
        &fixture_path_env,
        "tests/test_renamed.py",
    );
    let invalidated = string_set(&configured_execution["invalidated_partitions"]);
    assert!(invalidated.contains("python"));
    assert!(invalidated.contains("toml"));
    assert!(
        configured_execution["executed_graph_enrichment_operation_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        configured_execution["executed_readiness_validation_request_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_fresh_graph_has_no_path(&configured, "tests/test_app.py").await;
    let (configured_archive, configured_sidecar) = archive_case(
        &rna,
        &configured,
        &evidence,
        "configured",
        &fixture_path_env,
    );

    let deleted = clone_at(root, "deleted", &history.bare, &history.deleted);
    let (deleted_report, deleted_execution) = injected_scan(
        &rna,
        &deleted,
        &configured_archive,
        &configured_sidecar,
        &history.bare,
        &fixture_path_env,
        "",
    );
    assert!(deleted_execution["changed_file_count"].as_u64().unwrap() > 0);
    assert_fresh_graph_has_no_path(&deleted, "src/app.py").await;
    assert!(
        deleted_report["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["path"] != "src/app.py")
    );
    assert!(
        deleted_report["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|evidence| evidence["path"] != "src/app.py")
    );
    let deleted_work = json_file(&deleted.join(".oh/.cache/lsp_pass1_work_items.json"));
    assert!(
        deleted_work["records"]
            .as_object()
            .unwrap()
            .values()
            .all(|record| record["file"] != "src/app.py"),
        "deleted work-ledger records must be purged"
    );
    archive_case(&rna, &deleted, &evidence, "deleted", &fixture_path_env);
}

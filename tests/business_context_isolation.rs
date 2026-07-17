use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{IndexAddOption, Repository, Signature};
use petgraph::Direction;
use repo_native_alignment::business_context::{BusinessContextAdmission, BusinessContextMode};
use repo_native_alignment::graph::index::GraphIndex;
use repo_native_alignment::graph::{ExtractionSource, NodeKind};
use repo_native_alignment::server::{GraphState, RnaHandler, ScanEnrichmentOptions};
use repo_native_alignment::service::{self, SearchContext, SearchParams};

const SENTINEL: &str = "quasar_context_isolation_783";
const ORDINARY_PATHS: &[&str] = &[
    "README.md",
    "docs/guide.md",
    "notes.md",
    "src/lib.rs",
    "tests/isolation.rs",
    "config.toml",
];

fn assert_rst_is_not_business_context_filtered() {
    let admission = BusinessContextAdmission::new(BusinessContextMode::Disabled);
    let mut producer_paths = vec![
        PathBuf::from("reference.rst"),
        PathBuf::from(".oh/outcomes/leak.md"),
    ];

    assert_eq!(admission.retain_repository_files(&mut producer_paths), 1);
    assert_eq!(producer_paths, vec![PathBuf::from("reference.rst")]);
    assert_eq!(admission.counts().business_artifact_files, 1);
}

fn copy_fixture(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn write_index_tree(repo: &Repository) -> git2::Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    index.write_tree().unwrap()
}

fn initialize_merge_history(root: &Path) {
    let repo = Repository::init(root).unwrap();
    let signature = Signature::now("RNA fixture", "rna-fixture@example.invalid").unwrap();

    let initial_tree_id = write_index_tree(&repo);
    let initial_tree = repo.find_tree(initial_tree_id).unwrap();
    let initial_oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "initial fixture",
            &initial_tree,
            &[],
        )
        .unwrap();

    fs::write(root.join("history.txt"), format!("{SENTINEL}\n")).unwrap();
    let feature_tree_id = write_index_tree(&repo);
    let feature_tree = repo.find_tree(feature_tree_id).unwrap();
    let initial_commit = repo.find_commit(initial_oid).unwrap();
    let feature_oid = repo
        .commit(
            None,
            &signature,
            &signature,
            &format!("feature containing {SENTINEL}"),
            &feature_tree,
            &[&initial_commit],
        )
        .unwrap();
    let feature_commit = repo.find_commit(feature_oid).unwrap();

    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &format!("Merge pull request #783 from fixture/context [{SENTINEL}]"),
        &feature_tree,
        &[&initial_commit, &feature_commit],
    )
    .unwrap();
}

fn disabled_handler(root: &Path) -> RnaHandler {
    RnaHandler {
        repo_root: root.to_path_buf(),
        business_context: BusinessContextAdmission::new(BusinessContextMode::Disabled),
        ..RnaHandler::default()
    }
}

fn path_has_dot_oh(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == OsStr::new(".oh"))
}

async fn build_disabled(root: &Path) -> (RnaHandler, repo_native_alignment::server::GraphState) {
    let handler = disabled_handler(root);
    let graph = handler
        .build_full_graph_inner(false, ScanEnrichmentOptions::extract_only())
        .await
        .unwrap();
    (handler, graph)
}

fn assert_isolated_graph(graph: &repo_native_alignment::server::GraphState) {
    for expected in ORDINARY_PATHS {
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.id.file == PathBuf::from(expected)),
            "ordinary fixture path {expected} was not indexed"
        );
    }

    assert!(
        graph
            .nodes
            .iter()
            .all(|node| !path_has_dot_oh(&node.id.file)),
        ".oh business artifacts must never reach the disabled graph"
    );
    assert!(
        graph
            .nodes
            .iter()
            .all(|node| node.id.kind != NodeKind::PrMerge),
        "Git PR-history producer must not emit disabled-mode nodes"
    );
    assert!(
        graph
            .nodes
            .iter()
            .all(|node| node.source != ExtractionSource::Git),
        "Git-history provenance must not reach the disabled graph"
    );
    assert!(
        graph.nodes.iter().any(|node| {
            node.id.file == PathBuf::from("docs/guide.md")
                && (node.body.contains(SENTINEL) || node.signature.contains(SENTINEL))
        }),
        "ordinary nested documentation content must remain searchable"
    );

    let traversable = graph
        .nodes
        .iter()
        .find(|node| node.id.file == PathBuf::from("docs/guide.md"))
        .expect("nested guide node");
    assert!(
        !graph
            .index
            .neighbors(&traversable.stable_id(), None, Direction::Outgoing)
            .is_empty(),
        "ordinary nested documentation must remain graph-traversable"
    );
}

fn run_disabled_readme_query(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"))
        .args(["--business-context", "disabled", "search"])
        .arg("--repo")
        .arg(root)
        .args(["--file", "README.md", "--include-markdown", "--limit", "5"])
        .output()
        .unwrap()
}

#[tokio::test]
async fn disabled_mode_isolates_producers_and_rebuilds_incompatible_caches() {
    // Baseline RST extraction/LSP coverage is tracked in #784/#785. This
    // regression proves #783 adds no RST-specific producer exclusion.
    assert_rst_is_not_business_context_filtered();

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/business_context_isolation");
    let temp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture, temp.path());
    initialize_merge_history(temp.path());

    let (fresh_handler, fresh_graph) = build_disabled(temp.path()).await;
    assert_isolated_graph(&fresh_graph);
    let fresh_counts = fresh_handler.business_context.counts();
    assert!(fresh_counts.business_artifact_files >= 1);
    assert!(fresh_counts.git_history_producers >= 1);

    let cache = temp.path().join(".oh/.cache");
    assert_eq!(
        fs::read_to_string(cache.join("business-context-mode")).unwrap(),
        "disabled\n"
    );

    let same_mode_sentinel = cache.join("same-mode-sentinel");
    fs::write(&same_mode_sentinel, "preserve compatible cache").unwrap();
    let (_, reopened_graph) = build_disabled(temp.path()).await;
    assert!(same_mode_sentinel.exists());
    assert_isolated_graph(&reopened_graph);

    fs::write(cache.join("business-context-mode"), "enabled\n").unwrap();
    let mismatch_sentinel = cache.join("mismatch-sentinel");
    fs::write(&mismatch_sentinel, "must be discarded").unwrap();
    let (_, mismatch_graph) = build_disabled(temp.path()).await;
    assert!(!mismatch_sentinel.exists());
    assert_isolated_graph(&mismatch_graph);
    assert_eq!(
        fs::read_to_string(cache.join("business-context-mode")).unwrap(),
        "disabled\n"
    );

    fs::remove_file(cache.join("business-context-mode")).unwrap();
    let legacy_sentinel = cache.join("legacy-sentinel");
    fs::write(&legacy_sentinel, "must be discarded").unwrap();
    let (_, legacy_graph) = build_disabled(temp.path()).await;
    assert!(!legacy_sentinel.exists());
    assert_isolated_graph(&legacy_graph);
}

#[tokio::test]
async fn disabled_live_markdown_search_excludes_dot_oh_only() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/business_context_isolation");
    let temp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture, temp.path());

    let graph_state = GraphState::new(
        Vec::new(),
        Vec::new(),
        GraphIndex::new(),
        None,
        std::collections::HashSet::new(),
    );
    let business_context = BusinessContextAdmission::new(BusinessContextMode::Disabled);
    let params = SearchParams {
        query: Some(SENTINEL.to_string()),
        include_artifacts: false,
        include_markdown: true,
        limit: Some(20),
        ..SearchParams::default()
    };
    let ctx = SearchContext {
        graph_state: &graph_state,
        embed_index: None,
        repo_root: temp.path(),
        lsp_status: None,
        embed_status: None,
        root_filter: None,
        non_code_slugs: std::collections::HashSet::new(),
        enrichment_jobs: Vec::new(),
        business_context: &business_context,
    };

    let result = service::search(&params, &ctx).await;

    assert!(
        result.contains("README.md"),
        "ordinary Markdown missing: {result}"
    );
    assert!(
        result.contains("docs/guide.md"),
        "nested Markdown missing: {result}"
    );
    assert!(
        !result.contains(".oh/outcomes/leak.md"),
        "business artifact escaped live Markdown admission: {result}"
    );
    assert!(business_context.counts().business_artifact_files >= 1);
}

#[test]
fn direct_disabled_cli_query_rebuilds_mismatched_and_legacy_caches_first() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/business_context_isolation");
    let temp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture, temp.path());
    initialize_merge_history(temp.path());

    let cache = temp.path().join(".oh/.cache");
    for (case, persisted_mode) in [("mismatch", Some("enabled\n")), ("legacy", None)] {
        fs::create_dir_all(&cache).unwrap();
        let marker = cache.join("business-context-mode");
        match persisted_mode {
            Some(mode) => fs::write(&marker, mode).unwrap(),
            None if marker.exists() => fs::remove_file(&marker).unwrap(),
            None => {}
        }
        let poison = cache.join(format!("{case}-query-poison"));
        fs::write(&poison, "must be deleted before query").unwrap();

        let output = run_disabled_readme_query(temp.path());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{case} direct query failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(!poison.exists(), "{case} cache was read without rebuild");
        assert_eq!(fs::read_to_string(&marker).unwrap(), "disabled\n");
        assert!(
            stdout.contains("README.md"),
            "{case} rebuild did not serve ordinary README content:\n{stdout}"
        );
        assert!(!stdout.contains(".oh/outcomes/leak.md"));
    }
}

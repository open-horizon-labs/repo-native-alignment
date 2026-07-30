use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use repo_native_alignment::business_context::{BusinessContextAdmission, BusinessContextMode};
use repo_native_alignment::graph::index::GraphIndex;
use repo_native_alignment::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};
use repo_native_alignment::server::{GraphState, RnaHandler, load_graph_from_lance};

const RANK9_NODE_COUNT: usize = 301_300;
const RANK9_EDGE_COUNT: usize = 535_850;
const NORMAL_NODE_COUNT: usize = 4_947;
const NORMAL_EDGE_COUNT: usize = 9_537;
const MAX_WARM_LOAD: Duration = Duration::from_secs(30);
const MAX_CLI_QUERY: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 8 * 1024;

fn node(name: String) -> Node {
    Node {
        id: NodeId {
            kind: NodeKind::Function,
            name,
            file: PathBuf::from("src/rank9_fixture.rs"),
            root: "fixture".to_string(),
        },
        language: "rust".to_string(),
        signature: String::new(),
        line_start: 1,
        line_end: 1,
        body: String::new(),
        metadata: BTreeMap::new(),
        source: ExtractionSource::TreeSitter,
    }
}

fn edge(from: &Node, to: &Node) -> Edge {
    Edge {
        from: from.id.clone(),
        to: to.id.clone(),
        kind: EdgeKind::Calls,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
        evidence: Vec::new(),
    }
}

fn sparse_sidecar(path: &std::path::Path, bytes: u64) {
    File::create(path)
        .and_then(|file| file.set_len(bytes))
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", path.display()));
}

fn output_with_timeout(command: &mut Command, timeout: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start exact CLI traversal");
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .expect("poll exact CLI traversal")
            .is_some()
        {
            return child
                .wait_with_output()
                .expect("collect exact CLI traversal output");
        }
        if started.elapsed() >= timeout {
            child
                .kill()
                .expect("terminate timed-out exact CLI traversal");
            let output = child
                .wait_with_output()
                .expect("collect timed-out exact CLI traversal output");
            panic!(
                "exact CLI traversal exceeded {timeout:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn profile_cli_startup() -> (Duration, Duration) {
    let mut samples = Vec::with_capacity(2);
    for _ in 0..2 {
        let started = Instant::now();
        let mut command = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"));
        command.arg("--version");
        let output = output_with_timeout(&mut command, MAX_CLI_QUERY);
        assert!(output.status.success(), "CLI startup probe failed");
        samples.push(started.elapsed());
    }
    (samples[0], samples[1])
}

async fn profile_persisted_cache_shape(
    label: &str,
    node_count: usize,
    edge_count: usize,
    large_sidecars: bool,
) {
    let repo = tempfile::tempdir().expect("temporary persisted-cache fixture");
    let handler = RnaHandler {
        repo_root: repo.path().to_path_buf(),
        business_context: BusinessContextAdmission::new(BusinessContextMode::Disabled),
        ..RnaHandler::default()
    };
    handler
        .prepare_business_context_cache()
        .expect("record disabled cache identity");

    let target = node("target".to_string());
    let callees = (0..9)
        .map(|index| node(format!("callee_{index}")))
        .collect::<Vec<_>>();
    let target_edges = callees
        .iter()
        .map(|callee| edge(&target, callee))
        .collect::<Vec<_>>();
    let mut nodes = Vec::with_capacity(node_count);
    nodes.push(target.clone());
    nodes.extend(callees);
    for index in nodes.len()..node_count {
        nodes.push(node(format!("unrelated_{index}")));
    }
    let mut edges = Vec::with_capacity(edge_count);
    edges.extend(target_edges);
    let unrelated = &nodes[10..];
    for index in 0..(edge_count - edges.len()) {
        let from = index % unrelated.len();
        let hop = 1 + index / unrelated.len();
        let to = (from + hop) % unrelated.len();
        edges.push(edge(&unrelated[from], &unrelated[to]));
    }
    let mut index = GraphIndex::new();
    index.rebuild_from_edges(&edges);
    for graph_node in &nodes {
        index.ensure_node(&graph_node.stable_id(), &graph_node.id.kind.to_string());
    }
    let graph = GraphState::new(nodes, edges, index, None, HashSet::new());
    handler
        .persist_graph_snapshot(&graph)
        .await
        .unwrap_or_else(|error| panic!("persist {label} graph cache: {error:#}"));
    drop(graph);

    let cache = repo.path().join(".oh/.cache");
    let sidecar_megabytes = if large_sidecars {
        [147_u64, 176, 119]
    } else {
        [1_u64, 1, 1]
    };
    let sidecars = [
        (
            cache.join("enrichment_jobs.json"),
            sidecar_megabytes[0] * 1024 * 1024,
        ),
        (
            cache.join("lsp_completeness.json"),
            sidecar_megabytes[1] * 1024 * 1024,
        ),
        (
            cache.join("lsp_pass1_work_items.json"),
            sidecar_megabytes[2] * 1024 * 1024,
        ),
    ];
    for (path, bytes) in &sidecars {
        sparse_sidecar(path, *bytes);
    }
    fs::create_dir_all(repo.path().join("src")).expect("create source sentinel directory");
    fs::write(
        repo.path().join("src/unscanned_change.rs"),
        "pub fn must_not_replace_the_warm_cache() {}\n",
    )
    .expect("write source sentinel");

    let load_started = Instant::now();
    let loaded = load_graph_from_lance(repo.path())
        .await
        .expect("load persisted graph through production cache reader");
    let load_elapsed = load_started.elapsed();
    assert_eq!(loaded.nodes.len(), node_count);
    assert_eq!(loaded.edges.len(), edge_count);
    assert!(
        load_elapsed < MAX_WARM_LOAD,
        "{label} warm cache load took {load_elapsed:?}"
    );
    drop(loaded);

    let query_started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_repo-native-alignment"));
    command
        .args(["--business-context", "disabled", "search"])
        .arg("--repo")
        .arg(repo.path())
        .args([
            "--root",
            "all",
            "--compact",
            "--node",
            &target.stable_id(),
            "--mode",
            "neighbors",
            "--include-artifacts=false",
            "--include-markdown=false",
            "--limit",
            "20",
        ])
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1");
    let output = output_with_timeout(&mut command, MAX_CLI_QUERY);
    let query_elapsed = query_started.elapsed();
    assert!(
        output.status.success(),
        "warm traversal failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        query_elapsed < MAX_CLI_QUERY,
        "{label} warm CLI traversal took {query_elapsed:?}"
    );
    assert!(
        output.stdout.len() < MAX_OUTPUT_BYTES,
        "warm traversal emitted {} bytes",
        output.stdout.len()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("9 result(s)"), "got: {stdout}");
    assert!(!stdout.contains("Capability readiness"), "got: {stdout}");
    assert!(!stdout.contains("Enrichment jobs"), "got: {stdout}");

    let reopened = load_graph_from_lance(repo.path())
        .await
        .expect("reopen cache after CLI traversal");
    assert_eq!(
        reopened.nodes.len(),
        node_count,
        "query path rescanned or amplified the warm inventory"
    );
    assert_eq!(reopened.edges.len(), edge_count);
    assert!(
        reopened
            .nodes
            .iter()
            .all(|node| node.id.name != "must_not_replace_the_warm_cache"),
        "query path rescanned the source sentinel"
    );
    for (path, bytes) in sidecars {
        assert_eq!(
            fs::metadata(&path).expect("retained sidecar").len(),
            bytes,
            "query path rewrote irrelevant sidecar {}",
            path.display()
        );
    }
    eprintln!(
        "{label} warm cache profile: nodes={node_count}, edges={edge_count}, load={load_elapsed:?}, cli_query={query_elapsed:?}, output_bytes={}",
        output.stdout.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn normal_and_rank9_persisted_cache_traversal_is_bounded_and_does_not_rescan() {
    let (cold_startup, warm_startup) = profile_cli_startup();
    eprintln!("CLI startup phases: cold={cold_startup:?}, warm={warm_startup:?}");
    profile_persisted_cache_shape("normal", NORMAL_NODE_COUNT, NORMAL_EDGE_COUNT, false).await;
    profile_persisted_cache_shape("rank-9", RANK9_NODE_COUNT, RANK9_EDGE_COUNT, true).await;
}

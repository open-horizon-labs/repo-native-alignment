use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use gray_matter::{Matter, ParsedEntity, engine::YAML};
use serde::{Deserialize, Serialize};

const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const GENERATED_DIR: &str = ".oh/adr-validation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdrStatus {
    Proposed,
    Implementing,
    Implemented,
    Superseded,
}

impl AdrStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Proposed => "Proposed",
            Self::Implementing => "Implementing",
            Self::Implemented => "Implemented",
            Self::Superseded => "Superseded",
        }
    }
}

impl fmt::Display for AdrStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AdrValidationRefs {
    #[serde(default)]
    pub cargo_tests: Vec<String>,
    #[serde(default)]
    pub audits: Vec<String>,
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub smoke: Vec<String>,
}

impl AdrValidationRefs {
    fn total_checks(&self) -> usize {
        self.cargo_tests.len() + self.audits.len() + self.scripts.len() + self.smoke.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdrManifest {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub status: AdrStatus,
    pub source: String,
    pub validate: AdrValidationRefs,
}

#[derive(Debug, Deserialize)]
struct AdrFrontmatter {
    id: String,
    status: AdrStatus,
    #[serde(default)]
    validate: AdrValidationRefs,
}

#[derive(Debug, Clone)]
struct ParsedAdr {
    manifest: AdrManifest,
    source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompileReport {
    pub check: bool,
    pub manifests: Vec<String>,
    pub removed_stale: Vec<String>,
    pub readme_updated: bool,
    pub drift: Vec<String>,
}

impl CompileReport {
    pub fn ok(&self) -> bool {
        self.drift.is_empty()
    }

    pub fn human_summary(&self) -> String {
        if self.check {
            if self.ok() {
                return format!(
                    "ADR compile check passed: {} manifest(s) and README are in sync.",
                    self.manifests.len()
                );
            }
            return format!("ADR compile check failed:\n- {}", self.drift.join("\n- "));
        }

        let mut lines = vec![format!(
            "Compiled {} ADR manifest(s) into {}.",
            self.manifests.len(),
            GENERATED_DIR
        )];
        if self.readme_updated {
            lines.push("Updated docs/ADRs/README.md to match ADR source status.".to_string());
        }
        if !self.removed_stale.is_empty() {
            lines.push(format!(
                "Removed stale manifests: {}",
                self.removed_stale.join(", ")
            ));
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub manifest_count: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<AdrValidationResult>,
}

impl ValidationReport {
    pub fn ok(&self) -> bool {
        self.failed == 0
    }

    pub fn human_summary(&self) -> String {
        let mut lines = vec![format!(
            "ADR validation: {}/{} passed.",
            self.passed, self.manifest_count
        )];
        for result in &self.results {
            let verdict = if result.ok { "PASS" } else { "FAIL" };
            lines.push(format!("- {} [{}] {}", result.id, result.status, verdict));
            if !result.failures.is_empty() {
                for failure in &result.failures {
                    lines.push(format!("  - {}", failure));
                }
            }
        }
        lines.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdrValidationResult {
    pub id: String,
    pub title: String,
    pub status: AdrStatus,
    pub ok: bool,
    pub checks: Vec<CheckExecution>,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckExecution {
    pub kind: String,
    pub target: String,
    pub ok: bool,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditReport {
    pub name: String,
    pub ok: bool,
    pub details: String,
}

impl AuditReport {
    pub fn human_summary(&self) -> String {
        format!(
            "ADR audit {}: {}\n{}",
            self.name,
            if self.ok { "PASS" } else { "FAIL" },
            self.details
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidateSelection {
    pub id: Option<String>,
    pub source_path: Option<PathBuf>,
}

pub fn compile(repo_root: &Path, adr_dir: &Path, check: bool) -> Result<CompileReport> {
    let adrs = load_adrs(repo_root, adr_dir)?;
    let manifest_dir = repo_root.join(GENERATED_DIR);
    let mut manifests = Vec::new();
    let mut drift = Vec::new();
    let mut removed_stale = Vec::new();
    let mut readme_updated = false;

    if !check {
        fs::create_dir_all(&manifest_dir)
            .with_context(|| format!("creating {}", manifest_dir.display()))?;
    }

    let mut expected_files = BTreeSet::new();
    for adr in &adrs {
        let manifest_path = manifest_dir.join(format!("{}.json", adr.manifest.id));
        let rel_manifest_path = repo_relative_string(repo_root, &manifest_path)?;
        expected_files.insert(rel_manifest_path.clone());
        manifests.push(rel_manifest_path.clone());

        let rendered = render_manifest(&adr.manifest)?;
        if check {
            match fs::read_to_string(&manifest_path) {
                Ok(existing) => {
                    if existing != rendered {
                        drift.push(format!("manifest drift: {}", rel_manifest_path));
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    drift.push(format!("missing manifest: {}", rel_manifest_path));
                }
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("reading {}", manifest_path.display()));
                }
            }
        } else {
            fs::write(&manifest_path, rendered)
                .with_context(|| format!("writing {}", manifest_path.display()))?;
        }
    }

    if manifest_dir.is_dir() {
        for entry in fs::read_dir(&manifest_dir)
            .with_context(|| format!("reading {}", manifest_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let rel = repo_relative_string(repo_root, &path)?;
            if expected_files.contains(&rel) {
                continue;
            }
            if check {
                drift.push(format!("stale manifest: {}", rel));
            } else {
                fs::remove_file(&path)
                    .with_context(|| format!("removing stale manifest {}", path.display()))?;
                removed_stale.push(rel);
            }
        }
    }

    let readme_path = adr_dir.join("README.md");
    if readme_path.exists() {
        let rendered = render_readme(&adrs);
        if check {
            let existing = fs::read_to_string(&readme_path)
                .with_context(|| format!("reading {}", readme_path.display()))?;
            if existing != rendered {
                drift.push(format!(
                    "ADR README drift: {}",
                    repo_relative_string(repo_root, &readme_path)?
                ));
            }
        } else {
            let existing = fs::read_to_string(&readme_path).unwrap_or_default();
            if existing != rendered {
                fs::write(&readme_path, rendered)
                    .with_context(|| format!("writing {}", readme_path.display()))?;
                readme_updated = true;
            }
        }
    }

    Ok(CompileReport {
        check,
        manifests,
        removed_stale,
        readme_updated,
        drift,
    })
}

pub fn validate(
    repo_root: &Path,
    selection: &ValidateSelection,
    cargo_args: &[String],
) -> Result<ValidationReport> {
    let manifests = load_manifests(repo_root)?;
    let source_filter = selection
        .source_path
        .as_ref()
        .map(|path| repo_relative_string(repo_root, path))
        .transpose()?;

    let selected: Vec<_> = manifests
        .into_iter()
        .filter(|manifest| {
            selection.id.as_ref().is_none_or(|id| manifest.id == *id)
                && source_filter
                    .as_ref()
                    .is_none_or(|path| manifest.source == *path)
        })
        .collect();

    if selected.is_empty() {
        bail!(
            "no ADR manifests matched the requested selection under {}",
            repo_root.join(GENERATED_DIR).display()
        );
    }

    let requested_cargo_tests: BTreeSet<String> = selected
        .iter()
        .flat_map(|manifest| manifest.validate.cargo_tests.iter().cloned())
        .collect();
    let available_tests = if requested_cargo_tests.is_empty() {
        BTreeSet::new()
    } else {
        list_cargo_tests(repo_root, cargo_args)?
    };

    let mut results = Vec::new();
    for manifest in selected {
        let mut checks = Vec::new();
        let mut failures = Vec::new();

        if manifest.status == AdrStatus::Implemented && manifest.validate.total_checks() == 0 {
            failures.push("implemented ADR declares no executable validations".to_string());
        }

        for test_name in &manifest.validate.cargo_tests {
            if !available_tests.contains(test_name) {
                checks.push(CheckExecution {
                    kind: "cargo_test".to_string(),
                    target: test_name.clone(),
                    ok: false,
                    details: "not found in `cargo test -- --list` output".to_string(),
                });
                failures.push(format!("missing cargo test `{}`", test_name));
                continue;
            }

            let mut command = Command::new("cargo");
            command
                .arg("test")
                .args(cargo_args)
                .arg(test_name)
                .arg("--")
                .arg("--exact")
                .current_dir(repo_root);
            let output = run_command_with_timeout(&mut command, COMMAND_TIMEOUT)
                .with_context(|| format!("running cargo test {}", test_name))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let ran_exact_test =
                stdout.contains("running 1 test") || stderr.contains("running 1 test");
            let ok = output.success && ran_exact_test;
            checks.push(CheckExecution {
                kind: "cargo_test".to_string(),
                target: test_name.clone(),
                ok,
                details: if ok {
                    "passed".to_string()
                } else if output.timed_out {
                    format!("timed out after {}s", COMMAND_TIMEOUT.as_secs())
                } else if output.success {
                    "command succeeded but did not run exactly one test".to_string()
                } else {
                    summarize_command_output(&stdout, &stderr)
                },
            });
            if !ok {
                failures.push(format!("cargo test `{}` failed", test_name));
            }
        }

        for audit_name in &manifest.validate.audits {
            let audit = run_audit(repo_root, audit_name)?;
            checks.push(CheckExecution {
                kind: "audit".to_string(),
                target: audit_name.clone(),
                ok: audit.ok,
                details: audit.details.clone(),
            });
            if !audit.ok {
                failures.push(format!("audit `{}` failed", audit_name));
            }
        }

        for script in &manifest.validate.scripts {
            let script_path = repo_root.join(script);
            let mut command = if script_path.extension().and_then(|ext| ext.to_str()) == Some("sh")
            {
                let mut command = Command::new("bash");
                command.arg(&script_path);
                command
            } else {
                Command::new(&script_path)
            };
            command.current_dir(repo_root);
            let output = run_command_with_timeout(&mut command, COMMAND_TIMEOUT)
                .with_context(|| format!("running script {}", script))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let ok = output.success;
            checks.push(CheckExecution {
                kind: "script".to_string(),
                target: script.clone(),
                ok,
                details: if ok {
                    "passed".to_string()
                } else if output.timed_out {
                    format!("timed out after {}s", COMMAND_TIMEOUT.as_secs())
                } else {
                    summarize_command_output(&stdout, &stderr)
                },
            });
            if !ok {
                failures.push(format!("script `{}` failed", script));
            }
        }

        for fixture in &manifest.validate.smoke {
            let binary =
                std::env::current_exe().context("resolving current executable for smoke run")?;
            let mut command = Command::new(&binary);
            command
                .arg("test")
                .arg("--repo")
                .arg(repo_root.join(fixture))
                .current_dir(repo_root);
            let output = run_command_with_timeout(&mut command, COMMAND_TIMEOUT)
                .with_context(|| format!("running smoke fixture {}", fixture))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let ok = output.success;
            checks.push(CheckExecution {
                kind: "smoke".to_string(),
                target: fixture.clone(),
                ok,
                details: if ok {
                    "passed".to_string()
                } else if output.timed_out {
                    format!("timed out after {}s", COMMAND_TIMEOUT.as_secs())
                } else {
                    summarize_command_output(&stdout, &stderr)
                },
            });
            if !ok {
                failures.push(format!("smoke fixture `{}` failed", fixture));
            }
        }

        let ok = failures.is_empty();
        results.push(AdrValidationResult {
            id: manifest.id,
            title: manifest.title,
            status: manifest.status,
            ok,
            checks,
            failures,
        });
    }

    let passed = results.iter().filter(|result| result.ok).count();
    let failed = results.len().saturating_sub(passed);
    Ok(ValidationReport {
        manifest_count: results.len(),
        passed,
        failed,
        results,
    })
}

#[derive(Debug)]
struct TimedCommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    success: bool,
    timed_out: bool,
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: std::time::Duration,
) -> Result<TimedCommandOutput> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning child process")?;
    let start = std::time::Instant::now();

    loop {
        if child.try_wait().context("polling child process")?.is_some() {
            let output = child
                .wait_with_output()
                .context("collecting child process output")?;
            return Ok(TimedCommandOutput {
                stdout: output.stdout,
                stderr: output.stderr,
                success: output.status.success(),
                timed_out: false,
            });
        }

        if start.elapsed() >= timeout {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .context("collecting timed-out child output")?;
            let mut stderr = output.stderr;
            if !stderr.is_empty() && !stderr.ends_with(b"\n") {
                stderr.push(b'\n');
            }
            stderr.extend_from_slice(
                format!("command timed out after {}s", timeout.as_secs()).as_bytes(),
            );
            return Ok(TimedCommandOutput {
                stdout: output.stdout,
                stderr,
                success: false,
                timed_out: true,
            });
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn run_audit(repo_root: &Path, name: &str) -> Result<AuditReport> {
    match name {
        "no_consumer_knows_other_consumers" => audit_no_consumer_knows_other_consumers(repo_root),
        "static_registration_only" => audit_static_registration_only(repo_root),
        "no_broker_logic_in_core" => audit_no_broker_logic_in_core(repo_root),
        "all_paths_go_through_event_bus" => audit_all_paths_go_through_event_bus(repo_root),
        "graph_state_uses_arcswap" => audit_graph_state_uses_arcswap(repo_root),
        other => bail!("unknown ADR audit `{}`", other),
    }
}

fn load_adrs(repo_root: &Path, adr_dir: &Path) -> Result<Vec<ParsedAdr>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(adr_dir)
        .with_context(|| format!("reading ADR directory {}", adr_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("README.md") {
            continue;
        }
        files.push(path);
    }
    files.sort();

    let mut adrs = Vec::new();
    for path in files {
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        adrs.push(parse_adr(repo_root, &path, &raw)?);
    }
    Ok(adrs)
}

fn parse_adr(repo_root: &Path, path: &Path, raw: &str) -> Result<ParsedAdr> {
    let matter: Matter<YAML> = Matter::new();
    let parsed: ParsedEntity = matter
        .parse(raw)
        .with_context(|| format!("parsing frontmatter in {}", path.display()))?;
    let has_frontmatter = !parsed.matter.is_empty()
        || (raw.trim_start().starts_with("---") && parsed.content.len() < raw.len());
    if !has_frontmatter {
        bail!("{} is missing YAML frontmatter", path.display());
    }

    let frontmatter: AdrFrontmatter = serde_yaml::from_str(&parsed.matter)
        .with_context(|| format!("decoding YAML frontmatter in {}", path.display()))?;
    let expected_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string();
    if frontmatter.id != expected_id {
        bail!(
            "{} frontmatter id `{}` must match file stem `{}`",
            path.display(),
            frontmatter.id,
            expected_id
        );
    }

    let title = extract_title(&parsed.content)
        .with_context(|| format!("extracting title from {}", path.display()))?;

    let validate = AdrValidationRefs {
        cargo_tests: dedup_preserve(frontmatter.validate.cargo_tests),
        audits: dedup_preserve(frontmatter.validate.audits),
        scripts: normalize_repo_paths(repo_root, &frontmatter.validate.scripts, "script")?,
        smoke: normalize_repo_paths(repo_root, &frontmatter.validate.smoke, "smoke fixture")?,
    };

    Ok(ParsedAdr {
        source_path: path.to_path_buf(),
        manifest: AdrManifest {
            schema_version: 1,
            id: frontmatter.id,
            title,
            status: frontmatter.status,
            source: repo_relative_string(repo_root, path)?,
            validate,
        },
    })
}

fn load_manifests(repo_root: &Path) -> Result<Vec<AdrManifest>> {
    let manifest_dir = repo_root.join(GENERATED_DIR);
    if !manifest_dir.is_dir() {
        bail!(
            "{} does not exist; run `repo-native-alignment adr compile --repo {}` first",
            manifest_dir.display(),
            repo_root.display()
        );
    }

    let mut manifests: Vec<AdrManifest> = Vec::new();
    for entry in fs::read_dir(&manifest_dir)
        .with_context(|| format!("reading {}", manifest_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        manifests.push(
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing manifest {}", path.display()))?,
        );
    }
    manifests.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(manifests)
}

fn render_manifest(manifest: &AdrManifest) -> Result<String> {
    let mut rendered =
        serde_json::to_string_pretty(manifest).context("serializing ADR manifest")?;
    rendered.push('\n');
    Ok(rendered)
}

fn render_readme(adrs: &[ParsedAdr]) -> String {
    let mut lines = vec![
        "# Architecture Decision Records".to_string(),
        String::new(),
        "| ADR | Title | Status |".to_string(),
        "|-----|-------|--------|".to_string(),
    ];
    for adr in adrs {
        let slug = adr
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let short_id = adr
            .manifest
            .id
            .split('-')
            .next()
            .unwrap_or(&adr.manifest.id);
        lines.push(format!(
            "| [{}]({}) | {} | {} |",
            short_id, slug, adr.manifest.title, adr.manifest.status
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn list_cargo_tests(repo_root: &Path, cargo_args: &[String]) -> Result<BTreeSet<String>> {
    let output = Command::new("cargo")
        .arg("test")
        .args(cargo_args)
        .arg("--")
        .arg("--list")
        .current_dir(repo_root)
        .output()
        .context("listing cargo tests for ADR validation")?;

    if !output.status.success() {
        bail!(
            "`cargo test -- --list` failed: {}",
            summarize_command_output(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr)
            )
        );
    }

    Ok(parse_cargo_test_list(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_cargo_test_list(stdout: &str) -> BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(ToOwned::to_owned)
        .collect()
}

fn audit_no_consumer_knows_other_consumers(repo_root: &Path) -> Result<AuditReport> {
    let extract_dir = repo_root.join("src/extract");
    let mut hits = Vec::new();
    for path in collect_rust_files(&extract_dir)? {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if file_name == "event_bus.rs" || file_name == "consumers.rs" {
            continue;
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        for needle in ["EventBus", "run_consumers", "PostExtractionRegistry"] {
            if content.contains(needle) {
                hits.push(format!(
                    "{} contains `{}`",
                    repo_relative_string(repo_root, &path)?,
                    needle
                ));
            }
        }
    }

    Ok(AuditReport {
        name: "no_consumer_knows_other_consumers".to_string(),
        ok: hits.is_empty(),
        details: if hits.is_empty() {
            "no consumer implementation imports EventBus/run_consumers/PostExtractionRegistry"
                .to_string()
        } else {
            hits.join("; ")
        },
    })
}

fn audit_static_registration_only(repo_root: &Path) -> Result<AuditReport> {
    let mut hits = Vec::new();
    for path in collect_rust_files(&repo_root.join("src"))? {
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut in_on_event = false;
        let mut depth: i32 = 0;
        for (idx, raw_line) in content.lines().enumerate() {
            let line = raw_line.trim();
            if !in_on_event && line.contains("fn on_event") {
                in_on_event = true;
                depth += brace_delta(line);
                continue;
            }
            if !in_on_event {
                continue;
            }
            if !line.starts_with("//")
                && (line.contains(".register(") || line.contains(".subscribe("))
            {
                hits.push(format!(
                    "{}:{} contains registration inside on_event",
                    repo_relative_string(repo_root, &path)?,
                    idx + 1
                ));
            }
            depth += brace_delta(line);
            if depth <= 0 {
                in_on_event = false;
                depth = 0;
            }
        }
    }

    Ok(AuditReport {
        name: "static_registration_only".to_string(),
        ok: hits.is_empty(),
        details: if hits.is_empty() {
            "no on_event handler performs dynamic registration/subscription".to_string()
        } else {
            hits.join("; ")
        },
    })
}

fn audit_no_broker_logic_in_core(repo_root: &Path) -> Result<AuditReport> {
    let mut hits = Vec::new();
    for dir in [repo_root.join("src/server"), repo_root.join("src/bus")] {
        if !dir.exists() {
            continue;
        }
        for path in collect_rust_files(&dir)? {
            let content =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            for (idx, raw_line) in content.lines().enumerate() {
                let line = raw_line.trim();
                if line.starts_with("//") {
                    continue;
                }
                let lower = line.to_ascii_lowercase();
                if [
                    "kafka", "pubsub", "rabbitmq", "rabbit", "celery", "pika", "redis",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
                {
                    hits.push(format!(
                        "{}:{} contains broker-specific core logic",
                        repo_relative_string(repo_root, &path)?,
                        idx + 1
                    ));
                }
            }
        }
    }

    Ok(AuditReport {
        name: "no_broker_logic_in_core".to_string(),
        ok: hits.is_empty(),
        details: if hits.is_empty() {
            "server/bus core contains no broker-specific logic".to_string()
        } else {
            hits.join("; ")
        },
    })
}

fn audit_all_paths_go_through_event_bus(repo_root: &Path) -> Result<AuditReport> {
    let needles = [
        "api_link_pass(",
        "tested_by_pass(",
        "import_calls_pass(",
        "directory_module_pass(",
        "framework_detection_pass(",
    ];
    let mut hits = Vec::new();
    for path in collect_rust_files(&repo_root.join("src/server"))? {
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        for (idx, raw_line) in content.lines().enumerate() {
            let line = raw_line.trim();
            if line.starts_with("//") {
                continue;
            }
            for needle in needles {
                if line.contains(needle) {
                    hits.push(format!(
                        "{}:{} contains direct pass call `{}`",
                        repo_relative_string(repo_root, &path)?,
                        idx + 1,
                        needle.trim_end_matches('(')
                    ));
                }
            }
        }
    }

    Ok(AuditReport {
        name: "all_paths_go_through_event_bus".to_string(),
        ok: hits.is_empty(),
        details: if hits.is_empty() {
            "server paths do not call post-extraction passes directly".to_string()
        } else {
            hits.join("; ")
        },
    })
}

fn audit_graph_state_uses_arcswap(repo_root: &Path) -> Result<AuditReport> {
    let path = repo_root.join("src/server/mod.rs");
    let content =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let non_comment = content
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let uses_arcswap = regex::Regex::new(
        r"pub\s+graph\s*:\s*Arc\s*<\s*ArcSwap\s*<\s*Option\s*<\s*Arc\s*<\s*GraphState\s*>\s*>\s*>\s*>",
    )
    .unwrap()
    .is_match(&non_comment);
    let still_has_rwlock = regex::Regex::new(
        r"Arc\s*<\s*RwLock\s*<\s*Option\s*<\s*GraphState\s*>\s*>\s*>|RwLock\s*<\s*Option\s*<\s*GraphState\s*>\s*>",
    )
    .unwrap()
    .is_match(&non_comment);

    Ok(AuditReport {
        name: "graph_state_uses_arcswap".to_string(),
        ok: uses_arcswap && !still_has_rwlock,
        details: if uses_arcswap && !still_has_rwlock {
            "RnaHandler.graph uses ArcSwap and no live RwLock GraphState remains".to_string()
        } else {
            "expected ArcSwap-backed graph field and no live RwLock GraphState".to_string()
        },
    })
}

fn collect_rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).with_context(|| format!("reading {}", next.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn extract_title(body: &str) -> Result<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return Ok(title.to_string());
            }
        }
    }
    bail!("ADR body is missing a level-1 heading")
}

fn normalize_repo_paths(repo_root: &Path, raw_paths: &[String], kind: &str) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in raw_paths {
        let path = normalize_repo_relative_path(raw)?;
        let full_path = repo_root.join(&path);
        if !full_path.exists() {
            bail!("{} `{}` does not exist", kind, path.display());
        }
        let rel = path_to_slash_string(&path);
        if seen.insert(rel.clone()) {
            normalized.push(rel);
        }
    }
    Ok(normalized)
}

fn normalize_repo_relative_path(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw.trim());
    if raw.trim().is_empty() {
        bail!("path reference must not be empty");
    }
    if path.is_absolute() {
        bail!("path reference `{}` must be repo-relative", raw);
    }
    let normalized = normalize_path(path);
    if normalized.as_os_str().is_empty() {
        bail!("path reference `{}` resolves to an empty path", raw);
    }
    if normalized.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        bail!("path reference `{}` escapes the repository root", raw);
    }
    Ok(normalized)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

fn repo_relative_string(repo_root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    Ok(path_to_slash_string(relative))
}

fn path_to_slash_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn dedup_preserve(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0, |acc, ch| match ch {
        '{' => acc + 1,
        '}' => acc - 1,
        _ => acc,
    })
}

fn summarize_command_output(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim());
    let summary = combined
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("command failed with no output");
    summary.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_adr(repo: &Path, file: &str, content: &str) {
        let path = repo.join("docs/ADRs").join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn sample_adr(id: &str, status: &str, title: &str, validate: &str) -> String {
        format!(
            "---\nid: {}\nstatus: {}\nvalidate:\n{}---\n\n# {}\n\nBody.\n",
            id, status, validate, title
        )
    }

    #[test]
    fn test_compile_writes_manifests_and_readme() {
        let tmp = TempDir::new().unwrap();
        write_adr(
            tmp.path(),
            "001-event-bus-extraction-pipeline.md",
            &sample_adr(
                "001-event-bus-extraction-pipeline",
                "implemented",
                "Event bus extraction pipeline",
                "  cargo_tests:\n    - extract::event_bus::tests::test_depth_first_ordering\n",
            ),
        );
        write_adr(
            tmp.path(),
            "002-arcswap-graph-concurrency.md",
            &sample_adr(
                "002-arcswap-graph-concurrency",
                "implementing",
                "ArcSwap for graph concurrency",
                "  audits:\n    - graph_state_uses_arcswap\n",
            ),
        );
        fs::write(tmp.path().join("docs/ADRs/README.md"), "stale\n").unwrap();

        let report = compile(tmp.path(), &tmp.path().join("docs/ADRs"), false).unwrap();
        assert!(report.ok());
        assert_eq!(report.manifests.len(), 2);
        assert!(report.readme_updated);
        assert!(
            tmp.path()
                .join(".oh/adr-validation/001-event-bus-extraction-pipeline.json")
                .exists()
        );
        let readme = fs::read_to_string(tmp.path().join("docs/ADRs/README.md")).unwrap();
        assert!(readme.contains("| [001](001-event-bus-extraction-pipeline.md) | Event bus extraction pipeline | Implemented |"));
    }

    #[test]
    fn test_compile_check_detects_manifest_and_readme_drift() {
        let tmp = TempDir::new().unwrap();
        write_adr(
            tmp.path(),
            "001-event-bus-extraction-pipeline.md",
            &sample_adr(
                "001-event-bus-extraction-pipeline",
                "implemented",
                "Event bus extraction pipeline",
                "  audits:\n    - no_consumer_knows_other_consumers\n",
            ),
        );
        fs::write(tmp.path().join("docs/ADRs/README.md"), "wrong\n").unwrap();
        compile(tmp.path(), &tmp.path().join("docs/ADRs"), false).unwrap();
        fs::write(
            tmp.path()
                .join(".oh/adr-validation/001-event-bus-extraction-pipeline.json"),
            "{}\n",
        )
        .unwrap();

        let report = compile(tmp.path(), &tmp.path().join("docs/ADRs"), true).unwrap();
        assert!(!report.ok());
        assert!(
            report
                .drift
                .iter()
                .any(|entry| entry.contains("manifest drift"))
        );
    }

    #[test]
    fn test_compile_removes_stale_manifest() {
        let tmp = TempDir::new().unwrap();
        write_adr(
            tmp.path(),
            "001-event-bus-extraction-pipeline.md",
            &sample_adr(
                "001-event-bus-extraction-pipeline",
                "implemented",
                "Event bus extraction pipeline",
                "  audits:\n    - no_consumer_knows_other_consumers\n",
            ),
        );
        fs::write(tmp.path().join("docs/ADRs/README.md"), "stale\n").unwrap();
        fs::create_dir_all(tmp.path().join(".oh/adr-validation")).unwrap();
        fs::write(tmp.path().join(".oh/adr-validation/stale.json"), "{}\n").unwrap();

        let report = compile(tmp.path(), &tmp.path().join("docs/ADRs"), false).unwrap();
        assert!(
            report
                .removed_stale
                .iter()
                .any(|entry| entry == ".oh/adr-validation/stale.json")
        );
        assert!(!tmp.path().join(".oh/adr-validation/stale.json").exists());
    }

    #[test]
    fn test_parse_adr_requires_matching_id() {
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join("docs/ADRs/001-event-bus-extraction-pipeline.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let err = parse_adr(
            tmp.path(),
            &path,
            &sample_adr(
                "wrong-id",
                "implemented",
                "Event bus extraction pipeline",
                "  audits:\n    - no_consumer_knows_other_consumers\n",
            ),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must match file stem"));
    }

    #[test]
    fn test_normalize_repo_relative_path_rejects_escape() {
        let err = normalize_repo_relative_path("../outside.sh").unwrap_err();
        assert!(err.to_string().contains("escapes the repository root"));
    }

    #[test]
    fn test_parse_cargo_test_list_extracts_exact_names() {
        let tests = parse_cargo_test_list(
            "server::tests::test_arcswap_readers_see_consistent_snapshots: test\nextract::event_bus::tests::test_depth_first_ordering: test\n",
        );
        assert!(tests.contains("server::tests::test_arcswap_readers_see_consistent_snapshots"));
        assert!(tests.contains("extract::event_bus::tests::test_depth_first_ordering"));
    }

    #[test]
    fn test_audit_static_registration_only_detects_dynamic_registration() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src/sample.rs");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(
            &src,
            "impl X {\n    async fn on_event(&self) {\n        bus.register(Box::new(Y));\n    }\n}\n",
        )
        .unwrap();

        let report = audit_static_registration_only(tmp.path()).unwrap();
        assert!(!report.ok);
        assert!(report.details.contains("registration inside on_event"));
    }
}

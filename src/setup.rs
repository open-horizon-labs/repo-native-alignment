use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};

/// Compile-time path to the RNA source tree (used for `cargo install --path`).
const RNA_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
const SOURCE_DECLARATION_VERSION: &str = "0.1.0";
const SOURCE_DECLARATION_KIND: &str = "code.workspace:v1";
const SOURCE_FACT_TYPE: &str = "code.workspace.bootstrap.health";

const DEFAULT_MCP_TIMEOUT_MS: u64 = 30_000;
const GITHUB_RELEASE_BASE: &str =
    "https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download";
// ─── CLI args ───────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct SetupArgs {
    /// Target project directory to configure (default: current directory)
    #[arg(long, default_value = ".")]
    pub project: PathBuf,

    /// Print planned actions without writing files or running installs
    #[arg(long)]
    pub dry_run: bool,

    /// Skip OH skills installation step
    #[arg(long)]
    pub skip_skills: bool,

    /// Skip post-setup binary and config verification
    #[arg(long)]
    pub skip_verify: bool,
}

// ─── Entry point ────────────────────────────────────────────────────────────

pub fn run(args: &SetupArgs) -> Result<()> {
    // Normalise project path.  In dry-run we tolerate non-existent paths by
    // falling back to `std::path::absolute` so callers can preview without
    // creating the directory first.
    let project_path = if args.dry_run {
        match args.project.canonicalize() {
            Ok(p) => p,
            Err(_) => std::path::absolute(&args.project).with_context(|| {
                format!("Cannot resolve project path: {}", args.project.display())
            })?,
        }
    } else {
        args.project.canonicalize().with_context(|| {
            format!(
                "Project path does not exist or is inaccessible: {}",
                args.project.display()
            )
        })?
    };

    if args.dry_run {
        println!("[dry-run] Setup for project: {}", project_path.display());
        println!("[dry-run] No files will be written; no commands will be run.\n");
    } else {
        println!(
            "Setting up RNA + OH MCP for project: {}",
            project_path.display()
        );
    }

    // ── Step 1: preflight ───────────────────────────────────────────────────
    println!("Checking dependencies...");
    let binary = installed_binary_path()?;
    let source_available = Path::new(RNA_MANIFEST_DIR).exists();

    if args.dry_run {
        if source_available {
            println!("[dry-run] Would check: cargo --version");
            println!("[dry-run] Would check: protoc --version");
        } else if binary.exists() {
            println!(
                "[dry-run] Would skip cargo/protoc checks (RNA source path missing; existing binary found at {})",
                binary.display()
            );
        } else {
            println!("[dry-run] Would check: curl --version");
            println!(
                "[dry-run] Would download release binary from GitHub Releases to {}",
                binary.display()
            );
        }

        if args.skip_skills {
            println!("[dry-run] Would skip: npx --version (--skip-skills)\n");
        } else {
            println!("[dry-run] Would check: npx --version\n");
        }
    } else {
        let needs_download = !source_available && !binary.exists();
        preflight(source_available, !args.skip_skills, needs_download)?;
    }

    // ── Step 2: install RNA binary ─────────────────────────────────────────
    if args.dry_run {
        if source_available {
            println!("[dry-run] Would run: cargo install --locked --path {RNA_MANIFEST_DIR}");
        } else if binary.exists() {
            println!(
                "[dry-run] Would skip RNA install (source path missing); existing binary: {}",
                binary.display()
            );
        } else {
            println!(
                "[dry-run] Would download release binary from {GITHUB_RELEASE_BASE} to {}",
                binary.display()
            );
        }
    } else {
        install_rna_binary(&binary)?;
    }

    // ── Step 3: install OH skills ──────────────────────────────────────────
    let skills_cmd = "npx skills add open-horizon-labs/skills -g -a claude-code -y";
    if args.dry_run {
        if args.skip_skills {
            println!("[dry-run] Would skip OH skills install (--skip-skills)");
        } else {
            println!("[dry-run] Would run: {skills_cmd}");
        }
    } else if args.skip_skills {
        println!("[skip] OH skills install skipped (--skip-skills)");
    } else {
        install_oh_skills()?;
    }

    // ── Step 4: merge .mcp.json ────────────────────────────────────────────
    let mcp_path = project_path.join(".mcp.json");
    if args.dry_run {
        println!("[dry-run] Would write/update: {}", mcp_path.display());
        println!("  mcpServers.rna-server.command = {}", binary.display());
        println!(
            "  mcpServers.rna-server.args    = [\"--repo\", \"{}\"]",
            project_path.display()
        );
    } else {
        merge_mcp_json_with_binary(&mcp_path, &project_path, &binary)?;
    }

    // ── Step 4.5: append MCP tool guidance to AGENTS.md ────────────────────
    let agents_md = project_path.join("AGENTS.md");
    if args.dry_run {
        if agents_md.exists() {
            println!("[dry-run] Would append RNA MCP tool guidance to AGENTS.md");
        } else {
            println!("[dry-run] No AGENTS.md found; skipping tool guidance injection");
        }
    } else if agents_md.exists() {
        inject_mcp_guidance(&agents_md)?;
    }

    // ── Step 5: initialize source boundary ──────────────────────────────────
    if args.dry_run {
        println!(
            "[dry-run] Would initialize source declaration: {}",
            source_declaration_path(&project_path).display()
        );
        println!(
            "[dry-run] Would initialize source outbox: {}",
            source_outbox_path(&project_path).display()
        );
        println!(
            "[dry-run] Would run replay smoke -> {}",
            source_projection_path(&project_path).display()
        );
    } else {
        initialize_source_boundary(&project_path)?;
    }

    // ── Step 6: verify ─────────────────────────────────────────────────────
    if args.dry_run {
        if args.skip_verify {
            println!("[dry-run] Would skip verification (--skip-verify)");
        } else {
            println!(
                "[dry-run] Would verify: binary --help, .mcp.json rna-server entry, source declaration valid, outbox writable, replay smoke passes"
            );
        }
    } else if args.skip_verify {
        println!("[skip] Verification skipped (--skip-verify)");
    } else {
        verify(&mcp_path, &project_path)?;
    }

    println!("\nSetup complete.");
    Ok(())
}

// ─── Preflight ───────────────────────────────────────────────────────────────

fn preflight(
    require_rna_install: bool,
    require_skills_install: bool,
    require_download: bool,
) -> Result<()> {
    if require_rna_install {
        check_dep("cargo", &["--version"], "Install Rust: https://rustup.rs")?;
        check_dep(
            "protoc",
            &["--version"],
            "Install protobuf:\n  macOS:  brew install protobuf\n  Linux:  https://grpc.io/docs/protoc-installation/",
        )?;
    }

    if require_download {
        check_dep(
            "curl",
            &["--version"],
            "Install curl:\n  macOS:  (pre-installed)\n  Linux:  apt-get install curl / yum install curl",
        )?;
    }

    if require_skills_install {
        check_dep(
            "npx",
            &["--version"],
            "Install Node.js (includes npx): https://nodejs.org",
        )?;
    }

    if !require_rna_install && !require_skills_install && !require_download {
        println!("  No dependency checks required.\n");
    } else {
        println!("  All required dependencies present.\n");
    }
    Ok(())
}

fn check_dep(cmd: &str, args: &[&str], remediation: &str) -> Result<()> {
    match Command::new(cmd).args(args).output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout);
            let ver = ver.trim();
            println!("  [ok] {cmd} ({ver})");
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "Preflight failed: `{cmd}` exited non-zero.\n  stderr: {}\n  Fix: {remediation}",
                stderr.trim()
            )
        }
        Err(_) => bail!("Preflight failed: `{cmd}` not found in PATH.\n  Fix: {remediation}"),
    }
}

// ─── Install steps ───────────────────────────────────────────────────────────

fn install_rna_binary(installed_binary: &Path) -> Result<()> {
    let source_path = Path::new(RNA_MANIFEST_DIR);
    if !source_path.exists() {
        if installed_binary.exists() {
            println!(
                "RNA source path not found at {}; using existing binary: {}\n",
                source_path.display(),
                installed_binary.display()
            );
            return Ok(());
        }

        // Source not available and no binary found -- download from GitHub Releases.
        println!("RNA source not available; downloading release binary...");
        download_rna_binary(installed_binary)?;
        return Ok(());
    }

    println!("Installing RNA binary (cargo install --locked --path {RNA_MANIFEST_DIR}) ...");
    let status = Command::new("cargo")
        .args(["install", "--locked", "--path", RNA_MANIFEST_DIR])
        .status()
        .context("Failed to launch `cargo install`")?;
    if !status.success() {
        bail!("`cargo install --locked --path {RNA_MANIFEST_DIR}` failed");
    }
    println!("  RNA binary installed.\n");
    Ok(())
}

/// Detect the current platform and download the appropriate release binary
/// from GitHub Releases into the directory containing `target_path`.
fn download_rna_binary(target_path: &Path) -> Result<()> {
    let (os, arch) = detect_platform()?;
    let asset_name = release_asset_name(&os, &arch)?;
    let url = format!("{GITHUB_RELEASE_BASE}/{asset_name}");

    // Ensure the target directory exists (typically ~/.cargo/bin/).
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create directory: {}", parent.display()))?;
    }

    println!("  Platform: {os}/{arch}");
    println!("  Downloading: {url}");
    println!("  Target: {}", target_path.display());

    let status = Command::new("curl")
        .args(["-fSL", &url])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("Failed to launch `curl`. Install curl or download the binary manually.")?
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture curl stdout"))?;

    let tar_status = Command::new("tar")
        .args([
            "xz",
            "-C",
            target_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_str()
                .context("Target directory path is not valid UTF-8")?,
        ])
        .stdin(status)
        .status()
        .context("Failed to launch `tar`")?;

    if !tar_status.success() {
        bail!(
            "Download failed. URL: {url}\n  \
             Your platform ({os}/{arch}) may not have a pre-built binary.\n  \
             Build from source instead: cargo install --locked --git https://github.com/open-horizon-labs/repo-native-alignment"
        );
    }

    // Verify the binary landed.
    if !target_path.exists() {
        bail!(
            "Download appeared to succeed but binary not found at {}.\n  \
             The release archive may have a different structure than expected.",
            target_path.display()
        );
    }

    println!("  RNA binary downloaded.\n");
    Ok(())
}

/// Returns (os, arch) strings matching `uname -s` / `uname -m`.
fn detect_platform() -> Result<(String, String)> {
    let os_out = Command::new("uname")
        .arg("-s")
        .output()
        .context("Failed to run `uname -s`")?;
    let arch_out = Command::new("uname")
        .arg("-m")
        .output()
        .context("Failed to run `uname -m`")?;

    let os = String::from_utf8_lossy(&os_out.stdout).trim().to_string();
    let arch = String::from_utf8_lossy(&arch_out.stdout).trim().to_string();
    Ok((os, arch))
}

/// Map platform to the GitHub Release asset filename.
/// Matches the naming convention used in CI release builds.
fn release_asset_name(os: &str, arch: &str) -> Result<String> {
    match (os, arch) {
        ("Darwin", "arm64") => {
            // Check for M2+ chips which have a -fast variant.
            if is_apple_m2_or_newer() {
                Ok("repo-native-alignment-darwin-arm64-fast.tar.gz".to_string())
            } else {
                Ok("repo-native-alignment-darwin-arm64.tar.gz".to_string())
            }
        }
        ("Linux", "x86_64") => Ok("repo-native-alignment-linux-x86_64.tar.gz".to_string()),
        _ => bail!(
            "No pre-built binary for {os}/{arch}.\n  \
             Build from source: cargo install --locked --git https://github.com/open-horizon-labs/repo-native-alignment"
        ),
    }
}

/// Detect Apple M2 or newer via sysctl. Returns false on non-macOS or M1.
fn is_apple_m2_or_newer() -> bool {
    let Ok(output) = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
    else {
        return false;
    };
    let brand = String::from_utf8_lossy(&output.stdout);
    let brand = brand.trim();
    // M2, M3, M4, etc.
    brand.contains("M2") || brand.contains("M3") || brand.contains("M4")
}

fn install_oh_skills() -> Result<()> {
    println!("Installing OH skills ...");
    let status = Command::new("npx")
        .args([
            "skills",
            "add",
            "open-horizon-labs/skills",
            "-g",
            "-a",
            "claude-code",
            "-y",
        ])
        .status()
        .context("Failed to launch `npx skills add`")?;
    if !status.success() {
        bail!("`npx skills add open-horizon-labs/skills` failed");
    }
    println!("  OH skills installed.\n");
    Ok(())
}

// ─── .mcp.json merge ─────────────────────────────────────────────────────────

fn installed_binary_path() -> Result<PathBuf> {
    if let Ok(root) = std::env::var("CARGO_INSTALL_ROOT")
        && !root.is_empty() {
            return Ok(PathBuf::from(root)
                .join("bin")
                .join("repo-native-alignment"));
        }

    if let Ok(home) = std::env::var("CARGO_HOME")
        && !home.is_empty() {
            return Ok(PathBuf::from(home)
                .join("bin")
                .join("repo-native-alignment"));
        }

    let home = std::env::var("HOME").context(
        "Cannot determine installed binary path; set CARGO_INSTALL_ROOT, CARGO_HOME, or HOME",
    )?;
    Ok(PathBuf::from(home).join(".cargo/bin/repo-native-alignment"))
}

/// Merge/create `.mcp.json`, setting the `rna-server` entry and preserving all
/// other keys.  Exposed as a separate function with explicit `binary_path` so
/// unit tests can call it without relying on the `HOME` env var.
pub fn merge_mcp_json_with_binary(
    mcp_path: &Path,
    project_path: &Path,
    binary_path: &Path,
) -> Result<()> {
    // Load existing file or start with an empty object.
    let mut root: Value = if mcp_path.exists() {
        let raw = std::fs::read_to_string(mcp_path)
            .with_context(|| format!("Cannot read {}", mcp_path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON", mcp_path.display()))?
    } else {
        json!({})
    };

    if !root.is_object() {
        bail!(
            "{}: root must be a JSON object, found: {}",
            mcp_path.display(),
            root
        );
    }

    // Ensure `mcpServers` exists and is an object.
    match root.get("mcpServers") {
        None => {
            root["mcpServers"] = json!({});
        }
        Some(v) if v.is_object() => {} // already valid
        Some(v) => bail!(
            "{}: `mcpServers` must be a JSON object, found: {}",
            mcp_path.display(),
            v
        ),
    }

    let command_path = binary_path
        .to_str()
        .with_context(|| format!("Binary path is not valid UTF-8: {}", binary_path.display()))?;
    let repo_path = project_path.to_str().with_context(|| {
        format!(
            "Project path is not valid UTF-8: {}",
            project_path.display()
        )
    })?;

    // Insert / overwrite the `rna-server` entry.
    root["mcpServers"]["rna-server"] = json!({
        "type": "stdio",
        "command": command_path,
        "args": ["--repo", repo_path],
        "timeout": DEFAULT_MCP_TIMEOUT_MS
    });

    let serialised =
        serde_json::to_string_pretty(&root).context("Failed to serialise .mcp.json")?;
    std::fs::write(mcp_path, serialised + "\n")
        .with_context(|| format!("Cannot write {}", mcp_path.display()))?;

    println!("Updated: {}", mcp_path.display());
    Ok(())
}

// ─── Source boundary initialization ─────────────────────────────────────────

fn source_declaration_path(project_path: &Path) -> PathBuf {
    project_path
        .join(".oh")
        .join("sources")
        .join("code.workspace.v1.json")
}

fn source_outbox_path(project_path: &Path) -> PathBuf {
    project_path
        .join(".oh")
        .join("outbox")
        .join("code.workspace.v1.ndjson")
}

fn source_projection_path(project_path: &Path) -> PathBuf {
    project_path
        .join(".oh")
        .join("projections")
        .join("code.workspace.v1.smoke.json")
}

fn initialize_source_boundary(project_path: &Path) -> Result<()> {
    let declaration_path = source_declaration_path(project_path);
    let outbox_path = source_outbox_path(project_path);
    let projection_path = source_projection_path(project_path);

    ensure_source_declaration(project_path, &declaration_path)?;
    ensure_outbox_initialized(&outbox_path)?;
    run_replay_smoke(project_path, &outbox_path, &projection_path)?;

    println!(
        "  Source declaration initialized: {}",
        declaration_path.display()
    );
    println!("  Source outbox initialized: {}", outbox_path.display());
    println!(
        "  Replay smoke projection updated: {}",
        projection_path.display()
    );
    Ok(())
}

fn ensure_source_declaration(project_path: &Path, declaration_path: &Path) -> Result<()> {
    if let Some(parent) = declaration_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }

    let canonical_root = project_path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize {}", project_path.display()))?;

    let expected = json!({
        "source": SOURCE_DECLARATION_KIND,
        "version": SOURCE_DECLARATION_VERSION,
        "repo_root": canonical_root.to_string_lossy(),
    });

    if !declaration_path.exists() {
        std::fs::write(
            declaration_path,
            serde_json::to_string_pretty(&expected)? + "\n",
        )
        .with_context(|| format!("Cannot write {}", declaration_path.display()))?;
        return Ok(());
    }

    let raw = std::fs::read_to_string(declaration_path)
        .with_context(|| format!("Cannot read {}", declaration_path.display()))?;
    let existing: Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", declaration_path.display()))?;

    if existing.get("source") != Some(&Value::String(SOURCE_DECLARATION_KIND.to_string())) {
        bail!(
            "{}: expected source '{}'",
            declaration_path.display(),
            SOURCE_DECLARATION_KIND
        );
    }

    if existing.get("version") != Some(&Value::String(SOURCE_DECLARATION_VERSION.to_string())) {
        bail!(
            "{}: expected version '{}'",
            declaration_path.display(),
            SOURCE_DECLARATION_VERSION
        );
    }

    Ok(())
}

fn ensure_outbox_initialized(outbox_path: &Path) -> Result<()> {
    if let Some(parent) = outbox_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }

    if !outbox_path.exists() {
        std::fs::File::create(outbox_path)
            .with_context(|| format!("Cannot create {}", outbox_path.display()))?;
    }

    std::fs::OpenOptions::new()
        .append(true)
        .open(outbox_path)
        .with_context(|| format!("Cannot open {} for append", outbox_path.display()))?;
    Ok(())
}

fn run_replay_smoke(project_path: &Path, outbox_path: &Path, projection_path: &Path) -> Result<()> {
    let canonical_root = project_path
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize {}", project_path.display()))?;

    let event_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("System clock before UNIX_EPOCH")?
        .as_secs();

    let canonical_record = json!({
        "source": SOURCE_DECLARATION_KIND,
        "version": SOURCE_DECLARATION_VERSION,
        "fact_type": SOURCE_FACT_TYPE,
        "repo_root": canonical_root.to_string_lossy(),
        "event_time": event_time
    });

    std::fs::write(
        outbox_path,
        serde_json::to_string(&canonical_record)? + "\n",
    )
    .with_context(|| format!("Cannot write {}", outbox_path.display()))?;

    let mut replayed = 0usize;
    let mut last_record: Option<Value> = None;
    let raw = std::fs::read_to_string(outbox_path)
        .with_context(|| format!("Cannot read {}", outbox_path.display()))?;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let parsed: Value = serde_json::from_str(line).with_context(|| {
            format!("Invalid canonical record line in {}", outbox_path.display())
        })?;
        replayed += 1;
        last_record = Some(parsed);
    }

    if replayed == 0 {
        bail!("Replay smoke failed: no canonical records to replay");
    }

    let projection = json!({
        "source": SOURCE_DECLARATION_KIND,
        "replayed_count": replayed,
        "last_fact_type": last_record
            .as_ref()
            .and_then(|value| value.get("fact_type"))
            .and_then(Value::as_str),
        "status": "ok"
    });

    if let Some(parent) = projection_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }

    std::fs::write(
        projection_path,
        serde_json::to_string_pretty(&projection)? + "\n",
    )
    .with_context(|| format!("Cannot write {}", projection_path.display()))?;
    Ok(())
}

// ─── Verification ────────────────────────────────────────────────────────────

fn verify(mcp_path: &Path, project_path: &Path) -> Result<()> {
    println!("\nVerifying setup...");
    let mut passed = true;

    // 1. Binary responds to --help.
    match installed_binary_path() {
        Err(e) => {
            println!("  [FAIL] Cannot locate installed binary: {e}");
            passed = false;
        }
        Ok(binary) => match Command::new(&binary).arg("--help").output() {
            Ok(out) if out.status.success() => {
                println!("  [PASS] Binary responds to --help: {}", binary.display());
            }
            Ok(out) => {
                println!("  [FAIL] Binary --help exited with: {}", out.status);
                passed = false;
            }
            Err(e) => {
                println!("  [FAIL] Cannot run {}: {e}", binary.display());
                passed = false;
            }
        },
    }

    // 2. .mcp.json contains rna-server entry.
    match std::fs::read_to_string(mcp_path) {
        Err(e) => {
            println!("  [FAIL] Cannot read {}: {e}", mcp_path.display());
            passed = false;
        }
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Err(e) => {
                println!("  [FAIL] {} is invalid JSON: {e}", mcp_path.display());
                passed = false;
            }
            Ok(v) if v.pointer("/mcpServers/rna-server").is_some() => {
                println!("  [PASS] .mcp.json contains rna-server entry");
            }
            Ok(_) => {
                println!("  [FAIL] .mcp.json missing rna-server entry");
                passed = false;
            }
        },
    }

    println!("  Source health:");

    let declaration_path = source_declaration_path(project_path);
    match ensure_source_declaration(project_path, &declaration_path) {
        Ok(_) => println!(
            "    [PASS] declaration valid ({})",
            declaration_path.display()
        ),
        Err(e) => {
            println!("    [FAIL] declaration invalid: {e}");
            passed = false;
        }
    }

    let outbox_path = source_outbox_path(project_path);
    match ensure_outbox_initialized(&outbox_path) {
        Ok(_) => println!("    [PASS] outbox writable ({})", outbox_path.display()),
        Err(e) => {
            println!("    [FAIL] outbox not writable: {e}");
            passed = false;
        }
    }

    let projection_path = source_projection_path(project_path);
    match run_replay_smoke(project_path, &outbox_path, &projection_path) {
        Ok(_) => println!(
            "    [PASS] replay smoke passed ({})",
            projection_path.display()
        ),
        Err(e) => {
            println!("    [FAIL] replay smoke failed: {e}");
            passed = false;
        }
    }

    if passed {
        println!("\nAll checks passed.");
        Ok(())
    } else {
        bail!("One or more verification checks failed (see above).");
    }
}

// ─── AGENTS.md MCP guidance injection ─────────────────────────────────────────

const MCP_GUIDANCE_MARKER: &str = "<!-- RNA MCP tool guidance -->";

const MCP_GUIDANCE_BLOCK: &str = r#"
<!-- RNA MCP tool guidance -->
## Code Exploration (use RNA MCP tools, not grep/Read)

| Instead of... | Use this MCP tool |
|---|---|
| `Grep` for symbol names | `search(query, kind, language, file)` |
| `Read` to trace function calls | `search(node: "<id>", mode: "neighbors")` |
| `Grep` for "who calls X" | `search(node: "<id>", mode: "impact")` |
| `Read` to find .oh/ artifacts | `search(query, include_artifacts=true)` |
| `Bash` with `grep -rn` | `search(query)` — searches code, artifacts, and markdown |
| Recording learnings/signals | Write to `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` (YAML frontmatter + markdown) |
| Searching git history | `search(query)` — returns commits; use `git show <hash>` via Bash for diffs |
<!-- end RNA MCP tool guidance -->
"#;

fn inject_mcp_guidance(agents_md: &Path) -> Result<()> {
    let content = std::fs::read_to_string(agents_md)
        .with_context(|| format!("Cannot read {}", agents_md.display()))?;

    if content.contains(MCP_GUIDANCE_MARKER) {
        println!("  AGENTS.md already has RNA MCP tool guidance; skipping.");
        return Ok(());
    }

    let updated = format!("{}\n{}", content.trim_end(), MCP_GUIDANCE_BLOCK);
    std::fs::write(agents_md, updated)
        .with_context(|| format!("Cannot write {}", agents_md.display()))?;

    println!("  Appended RNA MCP tool guidance to AGENTS.md");
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    fn tmp() -> (TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    fn read_json(path: &Path) -> Value {
        let raw = std::fs::read_to_string(path).expect("read");
        serde_json::from_str(&raw).expect("parse json")
    }

    // Helper binary path used in all merge tests – keeps tests env-var free.
    fn binary() -> PathBuf {
        PathBuf::from("/home/testuser/.cargo/bin/repo-native-alignment")
    }

    // ── create new config ────────────────────────────────────────────────────

    #[test]
    fn test_create_new_config() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");

        merge_mcp_json_with_binary(&mcp, Path::new("/my/project"), &binary()).unwrap();

        assert!(mcp.exists());
        let v = read_json(&mcp);
        assert_eq!(v["mcpServers"]["rna-server"]["type"], "stdio");
        assert_eq!(
            v["mcpServers"]["rna-server"]["command"],
            "/home/testuser/.cargo/bin/repo-native-alignment"
        );
        assert_eq!(v["mcpServers"]["rna-server"]["args"][0], "--repo");
        assert_eq!(v["mcpServers"]["rna-server"]["args"][1], "/my/project");
        assert_eq!(v["mcpServers"]["rna-server"]["timeout"], DEFAULT_MCP_TIMEOUT_MS);
    }

    // ── merge preserves other servers ────────────────────────────────────────

    #[test]
    fn test_merge_preserves_other_servers() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");

        std::fs::write(
            &mcp,
            r#"{
  "mcpServers": {
    "other-server": {
      "type": "stdio",
      "command": "/usr/local/bin/other"
    }
  }
}"#,
        )
        .unwrap();

        merge_mcp_json_with_binary(&mcp, Path::new("/proj"), &binary()).unwrap();

        let v = read_json(&mcp);
        // Existing server is untouched.
        assert_eq!(
            v["mcpServers"]["other-server"]["command"],
            "/usr/local/bin/other"
        );
        // New server is present.
        assert_eq!(v["mcpServers"]["rna-server"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["rna-server"]["args"][1], "/proj");
    }

    // ── rewrite stale rna-server entry ───────────────────────────────────────

    #[test]
    fn test_rewrite_stale_rna_server() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");

        std::fs::write(
            &mcp,
            r#"{
  "mcpServers": {
    "rna-server": {
      "type": "stdio",
      "command": "/old/path/bin",
      "args": ["--repo", "/old/project"],
      "timeout": 5000
    }
  }
}"#,
        )
        .unwrap();

        let new_binary = PathBuf::from("/new/bin/repo-native-alignment");
        merge_mcp_json_with_binary(&mcp, Path::new("/new/project"), &new_binary).unwrap();

        let v = read_json(&mcp);
        assert_eq!(
            v["mcpServers"]["rna-server"]["command"],
            "/new/bin/repo-native-alignment"
        );
        assert_eq!(v["mcpServers"]["rna-server"]["args"][1], "/new/project");
        assert_eq!(v["mcpServers"]["rna-server"]["timeout"], DEFAULT_MCP_TIMEOUT_MS);
    }

    // ── idempotent merge ─────────────────────────────────────────────────────

    #[test]
    fn test_idempotent_merge() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");

        merge_mcp_json_with_binary(&mcp, Path::new("/proj"), &binary()).unwrap();
        let first = std::fs::read_to_string(&mcp).unwrap();

        merge_mcp_json_with_binary(&mcp, Path::new("/proj"), &binary()).unwrap();
        let second = std::fs::read_to_string(&mcp).unwrap();

        assert_eq!(first, second, "merge must be idempotent");
    }

    // ── non-object root → error ───────────────────────────────────────────────

    #[test]
    fn test_non_object_root_errors() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");
        std::fs::write(&mcp, r#"["not", "an", "object"]"#).unwrap();

        let err = merge_mcp_json_with_binary(&mcp, Path::new("/p"), &binary())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("root must be a JSON object"),
            "unexpected error: {err}"
        );
    }

    // ── non-object mcpServers → error ─────────────────────────────────────────

    #[test]
    fn test_non_object_mcp_servers_errors() {
        let (_dir, proj) = tmp();
        let mcp = proj.join(".mcp.json");
        std::fs::write(&mcp, r#"{"mcpServers": "wrong"}"#).unwrap();

        let err = merge_mcp_json_with_binary(&mcp, Path::new("/p"), &binary())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`mcpServers` must be a JSON object"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_initialize_source_boundary_creates_artifacts() {
        let (_dir, proj) = tmp();

        initialize_source_boundary(&proj).unwrap();

        let declaration = source_declaration_path(&proj);
        let outbox = source_outbox_path(&proj);
        let projection = source_projection_path(&proj);

        assert!(declaration.exists(), "source declaration must exist");
        assert!(outbox.exists(), "outbox file must exist");
        assert!(projection.exists(), "projection file must exist");

        let declaration_json = read_json(&declaration);
        assert_eq!(declaration_json["source"], SOURCE_DECLARATION_KIND);
        assert_eq!(declaration_json["version"], SOURCE_DECLARATION_VERSION);

        let projection_json = read_json(&projection);
        assert_eq!(projection_json["status"], "ok");
        assert_eq!(projection_json["source"], SOURCE_DECLARATION_KIND);
    }

    #[test]
    fn test_source_declaration_validation_fails_on_wrong_source() {
        let (_dir, proj) = tmp();
        let declaration = source_declaration_path(&proj);
        std::fs::create_dir_all(declaration.parent().unwrap()).unwrap();
        std::fs::write(&declaration, r#"{"source":"wrong","version":"0.1.0"}"#).unwrap();

        let err = ensure_source_declaration(&proj, &declaration)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected source"), "unexpected error: {err}");
    }

    // ── release asset name mapping ──────────────────────────────────────────

    #[test]
    fn test_release_asset_name_linux_x86_64() {
        let name = release_asset_name("Linux", "x86_64").unwrap();
        assert_eq!(name, "repo-native-alignment-linux-x86_64.tar.gz");
    }

    #[test]
    fn test_release_asset_name_darwin_arm64() {
        // On non-macOS test machines, is_apple_m2_or_newer() returns false,
        // so this should return the base arm64 variant.
        let name = release_asset_name("Darwin", "arm64").unwrap();
        assert!(
            name == "repo-native-alignment-darwin-arm64.tar.gz"
                || name == "repo-native-alignment-darwin-arm64-fast.tar.gz",
            "unexpected asset name: {name}"
        );
    }

    #[test]
    fn test_release_asset_name_unsupported_platform() {
        let err = release_asset_name("Windows", "x86_64")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("No pre-built binary"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_detect_platform_returns_nonempty() {
        let (os, arch) = detect_platform().unwrap();
        assert!(!os.is_empty(), "OS should not be empty");
        assert!(!arch.is_empty(), "arch should not be empty");
    }
}

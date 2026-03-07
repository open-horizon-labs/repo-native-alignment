use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};

/// Compile-time path to the RNA source tree (used for `cargo install --path`).
const RNA_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

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
            println!(
                "[dry-run] Would fail without RNA source or existing binary at {}",
                binary.display()
            );
        }

        if args.skip_skills {
            println!("[dry-run] Would skip: npx --version (--skip-skills)\n");
        } else {
            println!("[dry-run] Would check: npx --version\n");
        }
    } else {
        preflight(source_available, !args.skip_skills)?;
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
                "[dry-run] Would fail install: source path missing and binary not found at {}",
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

    // ── Step 5: verify ─────────────────────────────────────────────────────
    if args.dry_run {
        if args.skip_verify {
            println!("[dry-run] Would skip verification (--skip-verify)");
        } else {
            println!("[dry-run] Would verify: binary --help, .mcp.json rna-server entry");
        }
    } else if args.skip_verify {
        println!("[skip] Verification skipped (--skip-verify)");
    } else {
        verify(&mcp_path)?;
    }

    println!("\nSetup complete.");
    Ok(())
}

// ─── Preflight ───────────────────────────────────────────────────────────────

fn preflight(require_rna_install: bool, require_skills_install: bool) -> Result<()> {
    if require_rna_install {
        check_dep("cargo", &["--version"], "Install Rust: https://rustup.rs")?;
        check_dep(
            "protoc",
            &["--version"],
            "Install protobuf:\n  macOS:  brew install protobuf\n  Linux:  https://grpc.io/docs/protoc-installation/",
        )?;
    }

    if require_skills_install {
        check_dep(
            "npx",
            &["--version"],
            "Install Node.js (includes npx): https://nodejs.org",
        )?;
    }

    if !require_rna_install && !require_skills_install {
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

        bail!(
            "RNA source path not found at {} and existing binary not found at {}. Re-clone repo-native-alignment or install a release binary first.",
            source_path.display(),
            installed_binary.display()
        );
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
    if let Ok(root) = std::env::var("CARGO_INSTALL_ROOT") {
        if !root.is_empty() {
            return Ok(PathBuf::from(root)
                .join("bin")
                .join("repo-native-alignment"));
        }
    }

    if let Ok(home) = std::env::var("CARGO_HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home)
                .join("bin")
                .join("repo-native-alignment"));
        }
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
        "timeout": 10000
    });

    let serialised =
        serde_json::to_string_pretty(&root).context("Failed to serialise .mcp.json")?;
    std::fs::write(mcp_path, serialised + "\n")
        .with_context(|| format!("Cannot write {}", mcp_path.display()))?;

    println!("Updated: {}", mcp_path.display());
    Ok(())
}

// ─── Verification ────────────────────────────────────────────────────────────

fn verify(mcp_path: &Path) -> Result<()> {
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
| `Grep` for symbol names | `search_symbols(query, kind, language, file)` |
| `Read` to trace function calls | `graph_neighbors(node_id, direction, edge_types)` |
| `Grep` for "who calls X" | `graph_impact(node_id, max_hops)` |
| `Read` to find .oh/ artifacts | `oh_search_context(query)` |
| `Bash` with `grep -rn` | `search_symbols` or `oh_search_context` |
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
        assert_eq!(v["mcpServers"]["rna-server"]["timeout"], 10000);
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
        assert_eq!(v["mcpServers"]["rna-server"]["timeout"], 10000);
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
}

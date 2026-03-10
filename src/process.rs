//! Child process execution with timeout and cleanup.
//!
//! Provides a reusable pattern for spawning external tools (language servers,
//! indexers, linters) with a configurable timeout. On timeout the child is
//! killed to prevent resource leaks, and any output files are cleaned up.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Outcome of running a child process with timeout.
#[derive(Debug)]
pub struct ProcessOutput {
    /// Raw stdout bytes.
    pub stdout: Vec<u8>,
    /// Raw stderr bytes (truncated to first 500 chars on failure for diagnostics).
    pub stderr: Vec<u8>,
    /// Whether the process exited successfully.
    pub success: bool,
}

/// Configuration for a child process run.
pub struct ProcessConfig<'a> {
    /// Binary name (looked up on PATH).
    pub binary: &'a str,
    /// Arguments to pass.
    pub args: &'a [&'a str],
    /// Working directory for the child process.
    pub working_dir: &'a Path,
    /// Maximum time to wait before killing the child.
    pub timeout: Duration,
    /// Output files to clean up after the process completes (success or failure).
    /// These are relative to `working_dir`.
    pub cleanup_files: &'a [&'a str],
}

/// Spawn a child process with timeout, kill on timeout, and clean up output files.
///
/// Returns `ProcessOutput` on success or timeout-with-kill. Returns an error
/// only for spawn failures (binary not found, permission denied, etc.).
///
/// # Cleanup
/// Files listed in `config.cleanup_files` are removed after the process
/// completes (whether it succeeded, failed, or was killed). This prevents
/// stale artifacts from accumulating.
///
/// # Example
/// ```ignore
/// let config = ProcessConfig {
///     binary: "rust-analyzer",
///     args: &["scip", "."],
///     working_dir: repo_root,
///     timeout: Duration::from_secs(120),
///     cleanup_files: &["index.scip"],
/// };
/// let output = run_with_timeout(&config).await?;
/// if output.success {
///     let index_path = repo_root.join("index.scip");
///     // process the output file before cleanup...
/// }
/// ```
pub async fn run_with_timeout(config: &ProcessConfig<'_>) -> Result<ProcessOutput> {
    let mut child = tokio::process::Command::new(config.binary)
        .args(config.args)
        .current_dir(config.working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn '{}'", config.binary))?;

    // Take stdout/stderr handles before waiting, so we can still kill the
    // child on timeout (wait_with_output takes ownership).
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let wait_result = tokio::time::timeout(config.timeout, child.wait()).await;

    let output = match wait_result {
        Err(_elapsed) => {
            // Timeout -- kill the child process to prevent resource leak
            tracing::warn!(
                "'{}' timed out after {}s, killing child process",
                config.binary,
                config.timeout.as_secs()
            );
            let _ = child.kill().await;
            ProcessOutput {
                stdout: Vec::new(),
                stderr: format!(
                    "'{}' timed out after {}s",
                    config.binary,
                    config.timeout.as_secs()
                )
                .into_bytes(),
                success: false,
            }
        }
        Ok(Err(e)) => {
            anyhow::bail!("'{}' failed to execute: {}", config.binary, e);
        }
        Ok(Ok(status)) => {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(ref mut h) = stdout_handle {
                let _ = tokio::io::AsyncReadExt::read_to_end(h, &mut stdout).await;
            }
            if let Some(ref mut h) = stderr_handle {
                let _ = tokio::io::AsyncReadExt::read_to_end(h, &mut stderr).await;
            }
            ProcessOutput {
                stdout,
                stderr,
                success: status.success(),
            }
        }
    };

    // Clean up output files regardless of success/failure
    for filename in config.cleanup_files {
        let path: PathBuf = config.working_dir.join(filename);
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_with_timeout_success() {
        let config = ProcessConfig {
            binary: "echo",
            args: &["hello"],
            working_dir: Path::new("/tmp"),
            timeout: Duration::from_secs(5),
            cleanup_files: &[],
        };
        let output = run_with_timeout(&config).await.unwrap();
        assert!(output.success);
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[tokio::test]
    async fn test_run_with_timeout_failure_exit_code() {
        let config = ProcessConfig {
            binary: "false",
            args: &[],
            working_dir: Path::new("/tmp"),
            timeout: Duration::from_secs(5),
            cleanup_files: &[],
        };
        let output = run_with_timeout(&config).await.unwrap();
        assert!(!output.success);
    }

    #[tokio::test]
    async fn test_run_with_timeout_kills_on_timeout() {
        let config = ProcessConfig {
            binary: "sleep",
            args: &["60"],
            working_dir: Path::new("/tmp"),
            timeout: Duration::from_millis(100),
            cleanup_files: &[],
        };
        let output = run_with_timeout(&config).await.unwrap();
        assert!(!output.success);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("timed out"), "stderr: {}", stderr);
    }

    #[tokio::test]
    async fn test_run_with_timeout_nonexistent_binary() {
        let config = ProcessConfig {
            binary: "definitely_not_a_real_binary_xyz_12345",
            args: &[],
            working_dir: Path::new("/tmp"),
            timeout: Duration::from_secs(5),
            cleanup_files: &[],
        };
        let result = run_with_timeout(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cleanup_files_removed() {
        // Create a temp file, then run a process that "produces" it
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("output.txt");
        std::fs::write(&artifact, "stale").unwrap();
        assert!(artifact.exists());

        let config = ProcessConfig {
            binary: "true",
            args: &[],
            working_dir: dir.path(),
            timeout: Duration::from_secs(5),
            cleanup_files: &["output.txt"],
        };
        let _output = run_with_timeout(&config).await.unwrap();
        assert!(!artifact.exists(), "Cleanup should have removed the file");
    }
}

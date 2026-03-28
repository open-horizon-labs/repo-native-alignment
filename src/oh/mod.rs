use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::types::{OhArtifact, OhArtifactKind};

/// Directory names under `.oh/` mapped to their artifact kinds.
const KIND_DIRS: &[(&str, OhArtifactKind)] = &[
    ("outcomes", OhArtifactKind::Outcome),
    ("signals", OhArtifactKind::Signal),
    ("guardrails", OhArtifactKind::Guardrail),
    ("metis", OhArtifactKind::Metis),
];

/// Scan all `.oh/` subdirectories and parse every `.md` file into an `OhArtifact`.
///
/// Missing directories are silently skipped (empty vec for that kind).
pub fn load_oh_artifacts(repo_root: &Path) -> Result<Vec<OhArtifact>> {
    let oh_dir = repo_root.join(".oh");
    let mut artifacts = Vec::new();

    for (dir_name, kind) in KIND_DIRS {
        let dir = oh_dir.join(dir_name);
        if !dir.is_dir() {
            continue;
        }

        let entries =
            fs::read_dir(&dir).with_context(|| format!("reading directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                match parse_artifact(&path, kind.clone()) {
                    Ok(artifact) => artifacts.push(artifact),
                    Err(e) => {
                        tracing::warn!("skipping {}: {:#}", path.display(), e);
                    }
                }
            }
        }
    }

    Ok(artifacts)
}

/// Parse a single `.md` file with YAML frontmatter into an `OhArtifact`.
///
/// Expects the file to start with `---`, followed by YAML, followed by a closing `---`,
/// then the markdown body. Files without frontmatter are treated as having an empty
/// frontmatter map.
pub fn parse_artifact(path: &Path, kind: OhArtifactKind) -> Result<OhArtifact> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let (frontmatter, body) = split_frontmatter(&raw);

    let fm: BTreeMap<String, serde_yaml::Value> = if frontmatter.is_empty() {
        BTreeMap::new()
    } else {
        serde_yaml::from_str(&frontmatter)
            .with_context(|| format!("parsing YAML frontmatter in {}", path.display()))?
    };

    Ok(OhArtifact {
        kind,
        file_path: path.to_path_buf(),
        frontmatter: fm,
        body,
    })
}

/// Write an artifact file at `.oh/{subdir}/{slug}.md`.
///
/// Creates the directory if it does not exist.
/// Returns the path to the newly created/updated file.
pub fn write_artifact(
    repo_root: &Path,
    subdir: &str,
    slug: &str,
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    body: &str,
) -> Result<PathBuf> {
    if slug.contains('/') || slug.contains('\\') || slug.contains("..") || slug.is_empty() {
        bail!(
            "invalid slug {:?}: must not be empty or contain '/', '\\', or '..'",
            slug
        );
    }

    let dir = repo_root.join(".oh").join(subdir);
    fs::create_dir_all(&dir).with_context(|| format!("creating directory {}", dir.display()))?;

    let file_path = dir.join(format!("{}.md", slug));

    let canonical_dir = dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", dir.display()))?;
    let canonical_file = canonical_dir.join(format!("{}.md", slug));
    if !canonical_file.starts_with(&canonical_dir) {
        bail!(
            "path traversal detected: {:?} escapes {}",
            slug,
            canonical_dir.display()
        );
    }

    let yaml = serde_yaml::to_string(frontmatter).context("serializing frontmatter to YAML")?;
    let yaml = yaml.trim_end();

    let content = format!("---\n{}\n---\n\n{}\n", yaml, body);

    fs::write(&file_path, &content).with_context(|| format!("writing {}", file_path.display()))?;

    Ok(file_path)
}

/// Convenience wrapper for backward compatibility.
pub fn write_metis(
    repo_root: &Path,
    slug: &str,
    frontmatter: &BTreeMap<String, serde_yaml::Value>,
    body: &str,
) -> Result<PathBuf> {
    write_artifact(repo_root, "metis", slug, frontmatter, body)
}

/// Update an existing artifact's frontmatter fields while preserving the body.
/// Only updates fields present in `updates`; other frontmatter fields are preserved.
pub fn update_artifact(
    repo_root: &Path,
    subdir: &str,
    slug: &str,
    updates: &BTreeMap<String, serde_yaml::Value>,
) -> Result<PathBuf> {
    let dir = repo_root.join(".oh").join(subdir);
    let file_path = dir.join(format!("{}.md", slug));

    if !file_path.exists() {
        bail!("{} not found at {}", slug, file_path.display());
    }

    let mut artifact = parse_artifact(&file_path, kind_for_subdir(subdir))?;

    for (k, v) in updates {
        artifact.frontmatter.insert(k.clone(), v.clone());
    }

    write_artifact(
        repo_root,
        subdir,
        slug,
        &artifact.frontmatter,
        &artifact.body,
    )
}

fn kind_for_subdir(subdir: &str) -> OhArtifactKind {
    match subdir {
        "outcomes" => OhArtifactKind::Outcome,
        "signals" => OhArtifactKind::Signal,
        "guardrails" => OhArtifactKind::Guardrail,
        "metis" => OhArtifactKind::Metis,
        _ => OhArtifactKind::Metis,
    }
}

/// Render a slice of artifacts as a single markdown document, grouped by kind.
pub fn artifacts_to_markdown(artifacts: &[OhArtifact]) -> String {
    let mut out = String::from("# Business Context (.oh/)\n\n");

    let groups: &[(OhArtifactKind, &str)] = &[
        (OhArtifactKind::Outcome, "Outcomes"),
        (OhArtifactKind::Signal, "Signals"),
        (OhArtifactKind::Guardrail, "Guardrails"),
        (OhArtifactKind::Metis, "Metis (Learnings)"),
    ];

    for (kind, heading) in groups {
        let matching: Vec<_> = artifacts.iter().filter(|a| a.kind == *kind).collect();
        if matching.is_empty() {
            continue;
        }

        out.push_str(&format!("## {}\n\n", heading));
        for artifact in matching {
            out.push_str(&artifact.to_markdown());
            out.push_str("\n---\n\n");
        }
    }

    if artifacts.is_empty() {
        out.push_str("_No .oh/ artifacts found._\n");
    }

    out
}

/// Split raw file content into (frontmatter_yaml, body).
///
/// If the file does not start with `---`, returns empty frontmatter and the
/// entire content as the body.
fn split_frontmatter(content: &str) -> (String, String) {
    let content = content.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }

    // Skip the opening `---` line
    let after_open = match trimmed.strip_prefix("---") {
        Some(rest) => rest.trim_start_matches(['\r', '\n']),
        None => return (String::new(), content.to_string()),
    };

    // Find the closing `---`
    if let Some(end) = after_open.find("\n---") {
        let yaml = after_open[..end].to_string();
        let rest = &after_open[end + 4..]; // skip "\n---"
        // Skip any trailing newlines after the closing ---
        let body = rest.trim_start_matches(['\r', '\n']).to_string();
        (yaml, body)
    } else {
        // No closing delimiter — treat entire content as body
        (String::new(), content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_frontmatter_basic() {
        let input = "---\nid: foo\nstatus: active\n---\n\n# Hello\n\nBody here.\n";
        let (fm, body) = split_frontmatter(input);
        assert_eq!(fm, "id: foo\nstatus: active");
        assert_eq!(body, "# Hello\n\nBody here.\n");
    }

    #[test]
    fn test_split_frontmatter_no_frontmatter() {
        let input = "# Just markdown\n\nNo frontmatter here.\n";
        let (fm, body) = split_frontmatter(input);
        assert!(fm.is_empty());
        assert_eq!(body, input);
    }

    #[test]
    fn test_split_frontmatter_no_closing() {
        let input = "---\nid: broken\nno closing delimiter\n";
        let (fm, body) = split_frontmatter(input);
        assert!(fm.is_empty());
        assert_eq!(body, input);
    }

    #[test]
    fn test_parse_frontmatter_yaml() {
        let yaml_str = "id: test-artifact\nstatus: active";
        let fm: BTreeMap<String, serde_yaml::Value> = serde_yaml::from_str(yaml_str).unwrap();
        assert_eq!(fm.get("id").unwrap().as_str().unwrap(), "test-artifact");
        assert_eq!(fm.get("status").unwrap().as_str().unwrap(), "active");
    }

    #[test]
    fn test_artifacts_to_markdown_empty() {
        let result = artifacts_to_markdown(&[]);
        assert!(result.contains("No .oh/ artifacts found."));
    }

    #[test]
    fn test_artifacts_to_markdown_grouped() {
        let artifacts = vec![
            OhArtifact {
                kind: OhArtifactKind::Outcome,
                file_path: PathBuf::from("test.md"),
                frontmatter: BTreeMap::from([(
                    "id".to_string(),
                    serde_yaml::Value::String("o1".to_string()),
                )]),
                body: "Outcome body".to_string(),
            },
            OhArtifact {
                kind: OhArtifactKind::Guardrail,
                file_path: PathBuf::from("g.md"),
                frontmatter: BTreeMap::from([(
                    "id".to_string(),
                    serde_yaml::Value::String("g1".to_string()),
                )]),
                body: "Guardrail body".to_string(),
            },
        ];
        let md = artifacts_to_markdown(&artifacts);
        assert!(md.contains("## Outcomes"));
        assert!(md.contains("## Guardrails"));
        assert!(!md.contains("## Signals"));
        assert!(!md.contains("## Metis"));
    }

    #[test]
    fn test_write_metis_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut fm = BTreeMap::new();
        fm.insert(
            "id".to_string(),
            serde_yaml::Value::String("test-entry".to_string()),
        );
        fm.insert(
            "outcome".to_string(),
            serde_yaml::Value::String("agent-alignment".to_string()),
        );

        let path = write_metis(root, "test-entry", &fm, "We learned something important.").unwrap();
        assert!(path.exists());

        let artifact = parse_artifact(&path, OhArtifactKind::Metis).unwrap();
        assert_eq!(artifact.id(), "test-entry");
        assert_eq!(
            artifact
                .frontmatter
                .get("outcome")
                .unwrap()
                .as_str()
                .unwrap(),
            "agent-alignment"
        );
        assert!(artifact.body.contains("We learned something important."));
    }
}

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingBackend {
    #[default]
    Auto,
    Cpu,
    Cuda,
    Metal,
}

impl fmt::Display for EmbeddingBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Metal => "metal",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FallbackPolicy {
    #[default]
    Cpu,
    Error,
}

impl fmt::Display for FallbackPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cpu => "cpu",
            Self::Error => "error",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingConfig {
    pub backend: EmbeddingBackend,
    pub cuda_device: usize,
    pub fallback: FallbackPolicy,
    pub batch_size: Option<usize>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: EmbeddingBackend::Auto,
            cuda_device: 0,
            fallback: FallbackPolicy::Cpu,
            batch_size: None,
        }
    }
}

impl EmbeddingConfig {
    pub fn from_repo(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join(".oh/config.toml");
        let mut config: Self = match std::fs::read_to_string(&path) {
            Ok(content) => {
                let value: toml::Value =
                    toml::from_str(&content).context("invalid .oh/config.toml")?;
                let table = value
                    .get("embeddings")
                    .cloned()
                    .unwrap_or(toml::Value::Table(Default::default()));
                table
                    .try_into()
                    .context("invalid [embeddings] configuration")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        config.apply_environment()?;
        config.validate()?;
        Ok(config)
    }

    pub fn apply_environment(&mut self) -> Result<()> {
        if let Ok(value) = std::env::var("RNA_EMBEDDING_BACKEND") {
            self.backend = value.parse().map_err(|_| {
                anyhow::anyhow!("RNA_EMBEDDING_BACKEND must be auto, cpu, cuda, or metal")
            })?;
        }
        if let Ok(value) = std::env::var("RNA_CUDA_DEVICE") {
            self.cuda_device = value
                .parse()
                .context("RNA_CUDA_DEVICE must be a non-negative integer")?;
        }
        if let Ok(value) = std::env::var("RNA_EMBEDDING_FALLBACK") {
            self.fallback = value
                .parse()
                .map_err(|_| anyhow::anyhow!("RNA_EMBEDDING_FALLBACK must be cpu or error"))?;
        }
        if let Ok(value) = std::env::var("RNA_EMBEDDING_BATCH_SIZE") {
            self.batch_size = Some(
                value
                    .parse()
                    .context("RNA_EMBEDDING_BATCH_SIZE must be a positive integer")?,
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.batch_size == Some(0) {
            bail!("embedding batch_size must be greater than zero");
        }
        if self.cuda_device > i32::MAX as usize {
            bail!(
                "embedding cuda_device {} exceeds the supported i32 range",
                self.cuda_device
            );
        }
        Ok(())
    }

    pub fn identity_flags(&self) -> [(&'static str, String); 5] {
        [
            ("embedding_backend", self.backend.to_string()),
            ("cuda_device", self.cuda_device.to_string()),
            ("embedding_fallback", self.fallback.to_string()),
            (
                "embedding_batch_size",
                self.batch_size
                    .map_or_else(|| "adaptive".into(), |n| n.to_string()),
            ),
            ("embedding_provider_contract", "onnxruntime-cuda-v1".into()),
        ]
    }
}

impl std::str::FromStr for EmbeddingBackend {
    type Err = ();
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "cuda" => Ok(Self::Cuda),
            "metal" => Ok(Self::Metal),
            _ => Err(()),
        }
    }
}

impl std::str::FromStr for FallbackPolicy {
    type Err = ();
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "cpu" => Ok(Self::Cpu),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_compatible_and_identity_is_explicit() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.backend, EmbeddingBackend::Auto);
        assert_eq!(config.identity_flags()[3].1, "adaptive");
    }

    #[test]
    fn invalid_batch_is_rejected() {
        let config = EmbeddingConfig {
            batch_size: Some(0),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn repository_configuration_controls_backend_device_fallback_and_batch() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".oh")).unwrap();
        std::fs::write(
            repo.path().join(".oh/config.toml"),
            "[embeddings]\nbackend = 'cuda'\ncuda_device = 1\nfallback = 'error'\nbatch_size = 32\n",
        )
        .unwrap();
        let config = EmbeddingConfig::from_repo(repo.path()).unwrap();
        assert_eq!(config.backend, EmbeddingBackend::Cuda);
        assert_eq!(config.cuda_device, 1);
        assert_eq!(config.fallback, FallbackPolicy::Error);
        assert_eq!(config.batch_size, Some(32));
    }
}

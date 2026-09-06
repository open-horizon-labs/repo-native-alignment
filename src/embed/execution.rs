//! Explicit encoder policy and the execution inputs shared by indexing and queries.
use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::generation::{DeviceAttestation, SemanticIdentity, sha256_bytes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    Auto,
    Cpu,
    OpenVino,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    pub backend: Backend,
    pub cpu_fallback: bool,
}

impl Policy {
    pub fn parse(
        backend: Option<&str>,
        fallback: Option<&str>,
        require_metal: bool,
    ) -> Result<Self> {
        let backend = match backend.unwrap_or("auto") {
            "auto" => Backend::Auto,
            "cpu" => Backend::Cpu,
            "openvino" => Backend::OpenVino,
            other => {
                bail!("invalid RNA_EMBEDDING_BACKEND {other:?}; expected auto, cpu, or openvino")
            }
        };
        let cpu_fallback = match fallback.unwrap_or("error") {
            "error" => false,
            "cpu" => true,
            other => bail!("invalid RNA_EMBEDDING_FALLBACK {other:?}; expected error or cpu"),
        };
        if require_metal && backend != Backend::Auto {
            bail!("strict Metal execution cannot be overridden by RNA_EMBEDDING_BACKEND");
        }
        Ok(Self {
            backend,
            cpu_fallback,
        })
    }

    pub fn from_env(require_metal: bool) -> Result<Self> {
        fn optional(name: &str) -> Result<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(anyhow::anyhow!("{name}: {error}")),
            }
        }
        Self::parse(
            optional("RNA_EMBEDDING_BACKEND")?.as_deref(),
            optional("RNA_EMBEDDING_FALLBACK")?.as_deref(),
            require_metal,
        )
    }

    pub fn requested(self) -> &'static str {
        match self.backend {
            Backend::Auto => "auto",
            Backend::Cpu => "cpu",
            Backend::OpenVino => "openvino",
        }
    }

    /// Run CPU only when explicitly allowed; retain the original failure for diagnostics.
    pub fn openvino_or_fallback<T>(
        self,
        provider: impl FnOnce() -> Result<T>,
        cpu: impl FnOnce(String) -> Result<T>,
    ) -> Result<T> {
        match provider() {
            Ok(value) => Ok(value),
            Err(error) if self.cpu_fallback => cpu(format!("{error:#}")),
            Err(error) => Err(error.context("OpenVINO unavailable; CPU fallback is disabled")),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExecutionIdentity {
    pub flags: BTreeMap<String, String>,
    pub assets: Option<BTreeMap<String, String>>,
}

impl ExecutionIdentity {
    pub fn new(policy: Policy, attestation: &DeviceAttestation, fallback: Option<&str>) -> Self {
        let mut value = Self::default();
        for (key, text) in [
            ("requested_backend", policy.requested()),
            (
                "fallback_policy",
                if policy.cpu_fallback { "cpu" } else { "error" },
            ),
            ("fallback_reason", fallback.unwrap_or("none")),
            ("provider", attestation.backend.as_str()),
            ("device", attestation.observed_device.as_str()),
        ] {
            value
                .flags
                .insert(format!("encoder_{key}"), text.to_string());
        }
        value
    }

    pub fn apply(&self, identity: &mut SemanticIdentity) -> Result<()> {
        identity.flags.retain(|key, _| !key.starts_with("encoder_"));
        identity.flags.extend(self.flags.clone());
        if let Some(assets) = &self.assets {
            identity.model = "Qdrant/all-MiniLM-L6-v2-onnx".into();
            identity.tokenizer = "fastembed-5.17.4:MiniLM:mean:l2:max_length=256".into();
            identity.model_files_digest = sha256_bytes(&serde_json::to_vec(assets)?);
            identity.model_sha256 = assets["model.onnx"].clone();
            identity.tokenizer_sha256 = assets["tokenizer.json"].clone();
        }
        Ok(())
    }

    pub fn validate_query(&self, identity: &SemanticIdentity) -> Result<()> {
        let mut observed = SemanticIdentity::for_current_process(384, identity.flags.clone())?;
        self.apply(&mut observed)?;
        if &observed != identity {
            bail!(
                "query encoder provider/device/policy/assets differ from the published generation; rebuild embeddings before semantic search"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openvino_policy_is_explicit_and_strict_metal_is_preserved() {
        assert_eq!(
            Policy::parse(None, None, false).unwrap().backend,
            Backend::Auto
        );
        assert_eq!(
            Policy::parse(Some("cpu"), None, false).unwrap().backend,
            Backend::Cpu
        );
        assert!(Policy::parse(Some("openvino"), None, true).is_err());
        assert!(Policy::parse(Some("typo"), None, false).is_err());
        assert!(Policy::parse(None, Some("silent"), false).is_err());
        assert!(
            !Policy::parse(Some("openvino"), None, false)
                .unwrap()
                .cpu_fallback
        );
    }
    #[test]
    fn openvino_fallback_requires_permission_and_retains_failure() {
        let policy = Policy::parse(Some("openvino"), None, false).unwrap();
        let failed = || Err::<String, _>(anyhow::anyhow!("Intel GPU.0 missing"));
        assert!(
            policy
                .openvino_or_fallback(failed, |_| panic!("CPU must not run"))
                .is_err()
        );
        let policy = Policy {
            cpu_fallback: true,
            ..policy
        };
        assert_eq!(
            policy.openvino_or_fallback(failed, Ok).unwrap(),
            "Intel GPU.0 missing"
        );
        assert_eq!(
            policy
                .openvino_or_fallback(|| Ok("gpu"), |_| panic!("CPU must not run"))
                .unwrap(),
            "gpu"
        );
    }
    #[test]
    fn openvino_identity_invalidates_reuse_and_query_on_each_execution_input() {
        let policy = Policy::parse(Some("openvino"), None, false).unwrap();
        let attestation = DeviceAttestation {
            required_device: "openvino".into(),
            observed_device: "Intel Arc GPU.0".into(),
            backend: "onnxruntime-openvino".into(),
            device_index: Some(0),
            artifact_sha256: "a".repeat(64),
        };
        let mut execution = ExecutionIdentity::new(policy, &attestation, None);
        execution.assets = Some(
            [
                "model.onnx",
                "tokenizer.json",
                "config.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
            ]
            .into_iter()
            .map(|name| (name.into(), sha256_bytes(name.as_bytes())))
            .collect(),
        );
        execution
            .flags
            .insert("encoder_runtime_digest".into(), "a".repeat(64));
        execution
            .flags
            .insert("encoder_precision".into(), "FP32".into());
        let mut identity = SemanticIdentity::for_current_process(384, BTreeMap::new()).unwrap();
        execution.apply(&mut identity).unwrap();
        execution.validate_query(&identity).unwrap();
        let digest = identity.digest().unwrap();
        for key in execution.flags.keys() {
            let mut changed = execution.clone();
            changed.flags.insert(key.clone(), "changed".into());
            assert!(changed.validate_query(&identity).is_err(), "{key}");
            let mut other = identity.clone();
            changed.apply(&mut other).unwrap();
            assert_ne!(other.digest().unwrap(), digest, "{key}");
        }
        for key in execution.assets.as_ref().unwrap().keys() {
            let mut changed = execution.clone();
            changed
                .assets
                .as_mut()
                .unwrap()
                .insert(key.clone(), "b".repeat(64));
            assert!(changed.validate_query(&identity).is_err(), "{key}");
            let mut other = identity.clone();
            changed.apply(&mut other).unwrap();
            assert_ne!(other.digest().unwrap(), digest, "{key}");
        }
        let mut other = identity.clone();
        other.dimension = 128;
        assert_ne!(other.digest().unwrap(), digest);
        let cpu = ExecutionIdentity::new(
            Policy {
                backend: Backend::Cpu,
                ..policy
            },
            &attestation,
            Some("provider unavailable"),
        );
        assert!(cpu.validate_query(&identity).is_err());
    }
}

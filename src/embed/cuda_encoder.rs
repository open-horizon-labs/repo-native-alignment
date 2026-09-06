//! Bounded, single-session CUDA MiniLM encoder.
//!
//! Integration: gate this module on `feature = "cuda"`; add optional direct
//! dependencies `tokenizers = "=0.22.2"`, `hf-hub = "=0.5.0"` (ureq + TLS),
//! and `tempfile = "3"` to that feature. No ndarray dependency is needed.
//! Tokenizer behavior below follows fastembed 5.17.2's private `common` loader.
//! Its exported TokenizerFiles is usable, but neither load_tokenizer nor the
//! Tokenizer type is re-exported. Never instantiate TextEmbedding for access.
//!
//! `RNA_MODEL_CACHE_DIR` defaults to the platform user cache directory's `rna`
//! subdirectory (normally ~/.cache/rna on Linux). `RNA_CUDA_PROFILE_DIR` can
//! override the platform temporary directory used for disposable profiles.
//! NUC deployments should set these overrides to their /srv/agent-data paths.
//! All five
//! assets come from one immutable snapshot of the same repository/file used by
//! fastembed's AllMiniLML6V2. Hashes describe bytes actually consumed, not a model
//! name or directory listing. Existing snapshots work without network access.
//!
//! Startup profiles real tokenized MiniLM runs on the retained session, requires
//! CUDA floating-point matrix/attention kernels, and rejects CPU compute except
//! bounded integer shape plumbing. `RNA_CUDA_STRICT_NO_CPU=1` additionally sets
//! session.disable_cpu_ep_fallback; failure never retries with another session.
//! Profiling ends after startup, so a long-running server cannot accumulate an
//! unbounded trace. Graph placement is fixed for this session. CPU tokenization,
//! attention-mask mean pooling, and normalization are intentional host work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use fastembed::{EmbeddingModel, TextEmbedding, TokenizerFiles};
use hf_hub::{Repo, RepoType, api::sync::ApiBuilder};
use ort::ep::{ArenaExtendStrategy, CUDA};
use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::{Tensor, TensorValueType};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokenizers::{AddedToken, PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

const REPOSITORY: &str = "Qdrant/all-MiniLM-L6-v2-onnx";
const MAX_LENGTH: usize = 512;
const DIMENSION: usize = 384;
const MAX_BATCH_SIZE: usize = 32;
// The arena limit excludes CUDA libraries' own allocations; it is not a total
// VRAM guarantee. Smaller callers' batches are honored, larger ones subdivided.
const ARENA_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Observations from this encoder's actual retained session, not a probe model.
#[derive(Debug, Clone)]
pub struct CudaExecutionEvidence {
    pub provider: String,
    /// CUDA logical ordinal read from the real output tensor's MemoryInfo.
    /// This is not an nvidia-smi physical ordinal or GPU UUID.
    pub device_id: usize,
    /// Output and profiled compute tensor type; not a hardware accumulator audit.
    pub precision: String,
    pub tf32_disabled: bool,
    pub cpu_fallback_disabled: bool,
    pub cuda_operations: BTreeMap<String, usize>,
    pub cpu_shape_operations: BTreeMap<String, usize>,
    pub profile_sha256: String,
}

pub struct CudaEncoder {
    session: Session,
    tokenizer: Tokenizer,
    output_name: String,
    needs_type_ids: bool,
    identity: BTreeMap<String, String>,
    evidence: CudaExecutionEvidence,
}

impl CudaEncoder {
    pub fn new(device_id: usize) -> Result<Self> {
        let ordinal = i32::try_from(device_id).context("CUDA device exceeds i32 range")?;
        let cache = match absolute_env_path("RNA_MODEL_CACHE_DIR")? {
            Some(path) => path,
            None => dirs::cache_dir()
                .context("cannot determine user cache directory; set RNA_MODEL_CACHE_DIR")?
                .join("rna"),
        };
        let (model_bytes, files, identity) = load_assets(&cache)?;
        let tokenizer = load_tokenizer(files)?;

        // Distinct prefixes prevent ORT's second-resolution timestamps colliding.
        // Declared before session so failure drops the session before its files.
        let scratch = match absolute_env_path("RNA_CUDA_PROFILE_DIR")? {
            Some(profile_root) => {
                fs::create_dir_all(&profile_root)
                    .context("create CUDA profiling scratch directory")?;
                tempfile::Builder::new()
                    .prefix("minilm-")
                    .tempdir_in(profile_root)?
            }
            None => tempfile::tempdir()?,
        };
        let strict = match std::env::var("RNA_CUDA_STRICT_NO_CPU") {
            Err(std::env::VarError::NotPresent) => false,
            Ok(v) if v == "0" => false,
            Ok(v) if v == "1" => true,
            _ => bail!("RNA_CUDA_STRICT_NO_CPU must be 0 or 1"),
        };
        // Recoverable builder errors contain a SessionBuilder; erase that payload
        // rather than requiring its internal pointers to satisfy anyhow's bounds.
        let builder_error =
            |e: ort::Error<ort::session::builder::SessionBuilder>| anyhow!(e.to_string());
        let mut builder = Session::builder()?
            .with_no_environment_execution_providers()
            .map_err(builder_error)?
            .with_execution_providers([CUDA::default()
                .with_device_id(ordinal)
                .with_tf32(false)
                .with_memory_limit(ARENA_BYTES)
                .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
                .build()
                .error_on_failure()])
            .map_err(builder_error)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(builder_error)?
            .with_intra_threads(1)
            .map_err(builder_error)?
            .with_parallel_execution(false)
            .map_err(builder_error)?
            .with_memory_pattern(false)
            .map_err(builder_error)?
            .with_profiling(scratch.path().join("execution"))
            .map_err(builder_error)?;
        if strict {
            builder = builder.with_disable_cpu_fallback().map_err(builder_error)?;
        }
        // Exactly these hashed bytes enter the only model session. No file can
        // change between hashing and loading, and no unrelated cache file enters
        // the identity. This self-contained model has no external initializers.
        let session = builder
            .commit_from_memory(&model_bytes)
            .context("create production MiniLM CUDA session")?;
        drop(model_bytes);
        let needs_type_ids = session
            .inputs()
            .iter()
            .any(|v| v.name() == "token_type_ids");
        for name in ["input_ids", "attention_mask"] {
            ensure!(
                session.inputs().iter().any(|v| v.name() == name),
                "missing MiniLM input {name}"
            );
        }
        ensure!(
            session.inputs().len() == if needs_type_ids { 3 } else { 2 },
            "unexpected MiniLM model inputs"
        );
        let output_name = if session.outputs().len() == 1 {
            session.outputs()[0].name().to_owned()
        } else {
            session
                .outputs()
                .iter()
                .find(|v| v.name() == "last_hidden_state")
                .context("MiniLM has no unambiguous token-level output")?
                .name()
                .to_owned()
        };
        let mut encoder = Self {
            session,
            tokenizer,
            output_name,
            needs_type_ids,
            identity,
            evidence: CudaExecutionEvidence {
                provider: String::new(),
                device_id,
                precision: String::new(),
                tf32_disabled: true,
                cpu_fallback_disabled: strict,
                cuda_operations: BTreeMap::new(),
                cpu_shape_operations: BTreeMap::new(),
                profile_sha256: String::new(),
            },
        };
        let observed_device = encoder.observe_output_device(ordinal)?;
        // Exercise the ordinary CPU-output path, including a padded batch and a
        // short query, on this SAME session before exposing an attestation.
        encoder.encode(
            vec![
                "query".into(),
                "A longer CUDA encoder verification passage.".into(),
            ],
            Some(2),
        )?;
        encoder.encode(vec!["short query".into()], Some(1))?;
        let profile_path = encoder
            .session
            .end_profiling()
            .context("finish CUDA execution profile")?;
        let profile = read_bounded(Path::new(&profile_path), 32 * 1024 * 1024)?;
        let (cuda_operations, cpu_shape_operations) = validate_profile(&profile)?;
        encoder.evidence = CudaExecutionEvidence {
            provider: "CUDAExecutionProvider".into(),
            device_id: observed_device,
            precision: "f32".into(),
            tf32_disabled: true,
            cpu_fallback_disabled: strict,
            cuda_operations,
            cpu_shape_operations,
            profile_sha256: digest(&profile),
        };
        Ok(encoder)
    }

    /// Stable content identity, deliberately excluding paths, device, and trace
    /// timestamps. Parent must include this in both build and query identity.
    pub fn asset_identity(&self) -> BTreeMap<String, String> {
        self.identity.clone()
    }

    pub fn execution_evidence(&self) -> &CudaExecutionEvidence {
        &self.evidence
    }

    pub fn encode(
        &mut self,
        texts: Vec<String>,
        batch_size: Option<usize>,
    ) -> Result<Vec<Vec<f32>>> {
        let requested = batch_size.unwrap_or(MAX_BATCH_SIZE);
        ensure!(requested > 0, "batch_size must be greater than zero");
        let mut result = Vec::with_capacity(texts.len());
        for batch in texts.chunks(requested.min(MAX_BATCH_SIZE)) {
            let input = self.tokenize(batch)?;
            let mut inputs = ort::inputs![
                "input_ids" => Tensor::from_array(([batch.len(), input.length], input.ids))?,
                "attention_mask" => Tensor::from_array(([batch.len(), input.length], input.mask.clone()))?,
            ];
            if self.needs_type_ids {
                inputs.push((
                    "token_type_ids".into(),
                    Tensor::from_array(([batch.len(), input.length], input.types))?.into(),
                ));
            }
            let outputs = self.session.run(inputs).context("MiniLM CUDA inference")?;
            let output = outputs
                .get(&self.output_name)
                .context("missing MiniLM output")?;
            let (shape, data) = output
                .try_extract_tensor::<f32>()
                .context("MiniLM output is not f32")?;
            ensure!(
                shape[..] == [batch.len() as i64, input.length as i64, DIMENSION as i64],
                "unexpected MiniLM output shape: {shape:?}"
            );
            result.extend(mean_normalize(
                data,
                &input.mask,
                batch.len(),
                input.length,
            )?);
        }
        Ok(result)
    }

    fn tokenize(&self, texts: &[String]) -> Result<Batch> {
        let encodings = self
            .tokenizer
            .encode_batch(texts.iter().map(String::as_str).collect(), true)
            .map_err(|e| anyhow!(e.to_string()))
            .context("tokenize MiniLM batch")?;
        ensure!(
            encodings.len() == texts.len(),
            "tokenizer batch count mismatch"
        );
        let length = encodings.first().context("empty tokenizer batch")?.len();
        ensure!(
            (1..=MAX_LENGTH).contains(&length),
            "invalid tokenized sequence length"
        );
        let mut input = Batch {
            length,
            ids: Vec::new(),
            mask: Vec::new(),
            types: Vec::new(),
        };
        for encoding in encodings {
            ensure!(
                encoding.len() == length
                    && encoding.get_attention_mask().len() == length
                    && encoding.get_type_ids().len() == length,
                "tokenizer did not pad consistently"
            );
            input
                .ids
                .extend(encoding.get_ids().iter().map(|&x| i64::from(x)));
            input
                .mask
                .extend(encoding.get_attention_mask().iter().map(|&x| i64::from(x)));
            input
                .types
                .extend(encoding.get_type_ids().iter().map(|&x| i64::from(x)));
        }
        Ok(input)
    }

    fn observe_output_device(&mut self, ordinal: i32) -> Result<usize> {
        let input =
            self.tokenize(&["Verify CUDA MiniLM execution on the selected device.".into()])?;
        let mut binding = self.session.create_binding()?;
        binding.bind_input(
            "input_ids",
            &Tensor::from_array(([1, input.length], input.ids))?,
        )?;
        binding.bind_input(
            "attention_mask",
            &Tensor::from_array(([1, input.length], input.mask))?,
        )?;
        if self.needs_type_ids {
            binding.bind_input(
                "token_type_ids",
                &Tensor::from_array(([1, input.length], input.types))?,
            )?;
        }
        binding.bind_output_to_device(
            &self.output_name,
            &MemoryInfo::new(
                AllocationDevice::CUDA,
                ordinal,
                AllocatorType::Device,
                MemoryType::Default,
            )?,
        )?;
        let outputs = self
            .session
            .run_binding(&binding)
            .context("run MiniLM with CUDA output binding")?;
        binding.synchronize_outputs()?;
        let tensor = outputs
            .get(&self.output_name)
            .context("missing CUDA-bound MiniLM output")?
            .downcast_ref::<TensorValueType<f32>>()?;
        let memory = tensor.memory_info();
        ensure!(
            memory.allocation_device() == AllocationDevice::CUDA && memory.device_id() == ordinal,
            "MiniLM output allocation does not match requested CUDA device"
        );
        // Allocation alone is NOT execution proof: validate_profile additionally
        // requires actual CUDA compute, with no floating-point CPU kernels.
        Ok(usize::try_from(memory.device_id())?)
    }
}

struct Batch {
    length: usize,
    ids: Vec<i64>,
    mask: Vec<i64>,
    types: Vec<i64>,
}

fn absolute_env_path(name: &str) -> Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os(name).map(PathBuf::from) else {
        return Ok(None);
    };
    ensure!(path.is_absolute(), "{name} must be an absolute path");
    Ok(Some(path))
}

fn digest(bytes: &[u8]) -> String {
    // Same raw, lowercase 64-character format as generation::sha256_bytes.
    format!("{:x}", Sha256::digest(bytes))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= limit,
        "invalid asset size: {}",
        path.display()
    );
    Ok(bytes)
}

fn load_assets(cache: &Path) -> Result<(Vec<u8>, TokenizerFiles, BTreeMap<String, String>)> {
    let info = TextEmbedding::get_model_info(&EmbeddingModel::AllMiniLML6V2)?;
    ensure!(
        info.model_code == REPOSITORY
            && info.model_file == "model.onnx"
            && info.dim == DIMENSION
            && info.additional_files.is_empty(),
        "fastembed MiniLM asset contract changed; review CUDA encoder before use"
    );
    let api = ApiBuilder::new()
        .with_cache_dir(cache.to_owned())
        .with_progress(false)
        .build()?;
    let model_path = api.model(REPOSITORY.into()).get("model.onnx")?;
    let snapshot = model_path
        .parent()
        .context("model has no snapshot directory")?;
    let revision = snapshot
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid snapshot revision")?;
    ensure!(
        revision.len() == 40 && revision.bytes().all(|c| c.is_ascii_hexdigit()),
        "HF cache did not resolve an immutable snapshot"
    );
    let repo = api.repo(Repo::with_revision(
        REPOSITORY.into(),
        RepoType::Model,
        revision.into(),
    ));
    let mut identity = BTreeMap::from([
        ("repository".into(), REPOSITORY.into()),
        ("revision".into(), revision.into()),
        ("pooling".into(), "attention-mask-mean".into()),
        ("normalization".into(), "l2-f32-epsilon-1e-12".into()),
        ("max_length".into(), MAX_LENGTH.to_string()),
        ("dimension".into(), DIMENSION.to_string()),
        ("precision".into(), "f32".into()),
    ]);
    let model = read_bounded(&model_path, 128 * 1024 * 1024)?;
    identity.insert("model.onnx".into(), digest(&model));
    let mut read = |name: &str| -> Result<Vec<u8>> {
        // hf-hub's get() for a SHA revision may require its own refs/<sha> file.
        // Reuse the already-resolved snapshot directly for offline cached assets.
        let local = snapshot.join(name);
        let path = if local.is_file() {
            local
        } else {
            repo.get(name)?
        };
        let bytes = read_bounded(&path, 16 * 1024 * 1024)?;
        identity.insert(name.into(), digest(&bytes));
        Ok(bytes)
    };
    let files = TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    };
    // Parent integration contract. Include exactly the files consumed by this
    // instance, in deterministic filename order, in the aggregate digest.
    let asset_hashes: BTreeMap<_, _> = [
        "model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ]
    .into_iter()
    .map(|name| (name, identity[name].as_str()))
    .collect();
    let model_files_digest = digest(&serde_json::to_vec(&asset_hashes)?);
    identity.insert("model".into(), REPOSITORY.into());
    identity.insert("tokenizer".into(), format!("{REPOSITORY}/tokenizer.json"));
    identity.insert("model_sha256".into(), identity["model.onnx"].clone());
    identity.insert(
        "tokenizer_sha256".into(),
        identity["tokenizer.json"].clone(),
    );
    identity.insert("model_files_digest".into(), model_files_digest);
    identity.insert("preprocessing_version".into(),
        "rna-minilm-v1:fastembed-5.17.2-tokenizer:max512:batch-longest:attention-mask-mean:l2-f32-eps1e-12".into());
    Ok((model, files, identity))
}

fn load_tokenizer(files: TokenizerFiles) -> Result<Tokenizer> {
    let config: Value = serde_json::from_slice(&files.config_file)?;
    let special: Value = serde_json::from_slice(&files.special_tokens_map_file)?;
    let settings: Value = serde_json::from_slice(&files.tokenizer_config_file)?;
    let model_max = settings["model_max_length"]
        .as_f64()
        .context("missing model_max_length")? as f32;
    ensure!(
        model_max >= MAX_LENGTH as f32,
        "MiniLM tokenizer cannot support max sequence 512"
    );
    let mut tokenizer =
        Tokenizer::from_bytes(files.tokenizer_file).map_err(|e| anyhow!(e.to_string()))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        pad_id: config["pad_token_id"].as_u64().unwrap_or(0) as u32,
        pad_token: settings["pad_token"]
            .as_str()
            .context("missing pad_token")?
            .into(),
        ..Default::default()
    }));
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_LENGTH.min(model_max as usize),
            ..Default::default()
        }))
        .map_err(|e| anyhow!(e.to_string()))?;
    if let Some(tokens) = special.as_object() {
        for value in tokens.values() {
            if let Some(content) = value.as_str() {
                tokenizer.add_special_tokens(&[AddedToken {
                    content: content.into(),
                    special: true,
                    ..Default::default()
                }]);
            } else if let (
                Some(content),
                Some(single_word),
                Some(lstrip),
                Some(rstrip),
                Some(normalized),
            ) = (
                value["content"].as_str(),
                value["single_word"].as_bool(),
                value["lstrip"].as_bool(),
                value["rstrip"].as_bool(),
                value["normalized"].as_bool(),
            ) {
                tokenizer.add_special_tokens(&[AddedToken {
                    content: content.into(),
                    special: true,
                    single_word,
                    lstrip,
                    rstrip,
                    normalized,
                }]);
            }
        }
    }
    Ok(tokenizer)
}

fn mean_normalize(data: &[f32], mask: &[i64], rows: usize, length: usize) -> Result<Vec<Vec<f32>>> {
    ensure!(
        length > 0 && mask.len() == rows * length && data.len() == mask.len() * DIMENSION,
        "invalid pooling dimensions"
    );
    ensure!(
        data.iter().all(|x| x.is_finite()),
        "MiniLM returned non-finite values"
    );
    let mut result = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut pooled = vec![0.0f32; DIMENSION];
        let mut count = 0.0f32;
        for token in 0..length {
            let index = row * length + token;
            ensure!(matches!(mask[index], 0 | 1), "non-binary attention mask");
            let weight = mask[index] as f32;
            count += weight;
            for (dst, src) in pooled
                .iter_mut()
                .zip(&data[index * DIMENSION..(index + 1) * DIMENSION])
            {
                *dst += weight * src;
            }
        }
        for x in &mut pooled {
            *x /= count.max(1.0);
        }
        let norm = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        ensure!(
            norm.is_finite() && norm > 0.0,
            "MiniLM produced an invalid zero/non-finite embedding"
        );
        for x in &mut pooled {
            *x /= norm + 1e-12;
        }
        result.push(pooled);
    }
    Ok(result)
}

type OperationCounts = BTreeMap<String, usize>;

fn validate_profile(bytes: &[u8]) -> Result<(OperationCounts, OperationCounts)> {
    let events: Vec<Value> = serde_json::from_slice(bytes).context("parse ORT profile")?;
    let mut cuda = BTreeMap::new();
    let mut cpu = BTreeMap::new();
    let mut compute_nodes = BTreeSet::new();
    for event in events {
        if event["cat"] != "Node" {
            continue;
        }
        let name = event["name"].as_str().context("unnamed ORT node event")?;
        if !name.ends_with("_kernel_time") {
            continue;
        }
        let args = &event["args"];
        let op = args["op_name"]
            .as_str()
            .context("ORT kernel lacks op_name")?;
        let provider = args["provider"]
            .as_str()
            .context("ORT kernel lacks provider evidence")?;
        let compute = matches!(
            op,
            "MatMul" | "Gemm" | "FusedMatMul" | "Attention" | "MultiHeadAttention"
        );
        match provider {
            "CUDAExecutionProvider" => {
                *cuda.entry(op.to_owned()).or_insert(0) += 1;
                if compute {
                    ensure!(
                        has_f32_tensor(&args["input_type_shape"])
                            && has_f32_tensor(&args["output_type_shape"]),
                        "CUDA compute lacks f32 tensor evidence: {name}"
                    );
                    compute_nodes.insert(name.to_owned());
                }
                // Copies and integer shape operations may carry integer tensors,
                // but no reduced-precision floating tensors are accepted.
                for field in ["input_type_shape", "output_type_shape"] {
                    if let Some(tensors) = args[field].as_array() {
                        for tensor in tensors {
                            if let Some(types) = tensor.as_object() {
                                ensure!(
                                    !types.keys().any(|t| matches!(
                                        t.as_str(),
                                        "float16" | "bfloat16" | "double"
                                    )),
                                    "unexpected CUDA precision in {name}"
                                );
                            }
                        }
                    }
                }
            }
            "CPUExecutionProvider" => {
                let shape_only = match op {
                    // Shape/Size inspect metadata, not floating tensor contents.
                    "Shape" | "Size" => small_integer_tensors(&args["output_type_shape"]),
                    "Gather" | "Slice" | "Unsqueeze" | "Squeeze" | "Concat" | "Reshape"
                    | "Cast" | "Add" | "Sub" | "Mul" | "Div" | "Equal" | "Where" | "Expand"
                    | "ConstantOfShape" | "Identity" | "ReduceProd" => {
                        small_integer_tensors(&args["input_type_shape"])
                            && small_integer_tensors(&args["output_type_shape"])
                    }
                    _ => false,
                };
                ensure!(
                    shape_only,
                    "CPU computation is not permitted in CUDA MiniLM: {name} ({op})"
                );
                *cpu.entry(op.to_owned()).or_insert(0) += 1;
            }
            _ => bail!("unexpected MiniLM execution provider {provider} at {name}"),
        }
    }
    // Require multiple distinct compute kernels in the completed MiniLM graph.
    // Counting unique names prevents repeated invocations of one trivial node
    // passing; this count alone does not identify transformer-layer boundaries.
    ensure!(
        compute_nodes.len() >= 6,
        "profile does not prove meaningful six-layer MiniLM CUDA compute"
    );
    Ok((cuda, cpu))
}

fn has_f32_tensor(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().any(|v| v.get("float").is_some()))
}

fn small_integer_tensors(value: &Value) -> bool {
    let Some(tensors) = value.as_array().filter(|v| !v.is_empty()) else {
        return false;
    };
    tensors.iter().all(|tensor| {
        let Some(types) = tensor.as_object().filter(|v| v.len() == 1) else {
            return false;
        };
        types.iter().all(|(ty, shape)| {
            if !matches!(ty.as_str(), "int64" | "int32" | "bool") {
                return false;
            }
            let Some(dims) = shape.as_array() else {
                return false;
            };
            // MiniLM shape vectors are tiny. Reject token-sized CPU arithmetic.
            dims.iter()
                .try_fold(1u64, |n, d| n.checked_mul(d.as_u64()?))
                .is_some_and(|n| n <= 64)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cuda_trace() -> Vec<Value> {
        (0..6)
            .map(|i| {
                json!({
                    "cat": "Node", "name": format!("matmul_{i}_kernel_time"),
                    "args": { "provider": "CUDAExecutionProvider", "op_name": "MatMul",
                        "input_type_shape": [{"float": [1, 4, 384]}, {"float": [384, 384]}],
                        "output_type_shape": [{"float": [1, 4, 384]}] }
                })
            })
            .collect()
    }

    fn check(events: &[Value]) -> Result<(OperationCounts, OperationCounts)> {
        validate_profile(&serde_json::to_vec(events)?)
    }

    #[test]
    fn execution_requires_compute_not_registration_copies_or_repetition() {
        assert!(check(&[]).is_err());
        let mut events = cuda_trace();
        for event in &mut events {
            event["args"]["op_name"] = json!("MemcpyFromHost");
        }
        assert!(check(&events).is_err());
        let single = cuda_trace().remove(0);
        assert!(check(&vec![single; 10]).is_err());
        assert_eq!(check(&cuda_trace()).unwrap().0["MatMul"], 6);
    }

    #[test]
    fn profile_rejects_cpu_float_compute_and_unidentified_providers() {
        let mut events = cuda_trace();
        let mut cpu = events[0].clone();
        cpu["args"]["provider"] = json!("CPUExecutionProvider");
        events.push(cpu);
        assert!(check(&events).is_err());
        events.last_mut().unwrap()["args"]["provider"] = json!("UnknownProvider");
        assert!(check(&events).is_err());
        events.last_mut().unwrap()["args"]
            .as_object_mut()
            .unwrap()
            .remove("provider");
        assert!(check(&events).is_err());
    }

    #[test]
    fn profile_permits_only_small_integer_cpu_plumbing() {
        let mut events = cuda_trace();
        events.push(json!({
            "cat": "Node", "name": "shape_kernel_time",
            "args": { "provider": "CPUExecutionProvider", "op_name": "Gather",
                "input_type_shape": [{"int64": [3]}, {"int64": []}],
                "output_type_shape": [{"int64": []}] }
        }));
        assert_eq!(check(&events).unwrap().1["Gather"], 1);
        events.last_mut().unwrap()["args"]["input_type_shape"] = json!([{"float": [3]}]);
        assert!(check(&events).is_err());
        events.last_mut().unwrap()["args"]["input_type_shape"] = json!([{"int64": [512]}]);
        assert!(check(&events).is_err());
        events.last_mut().unwrap()["args"]["input_type_shape"] = json!([]);
        assert!(check(&events).is_err());
    }

    #[test]
    fn profile_rejects_reduced_precision_compute() {
        let mut events = cuda_trace();
        events[0]["args"]["input_type_shape"] = json!([{"float16": [384, 384]}]);
        assert!(check(&events).is_err());
        events[0]["args"]["input_type_shape"] = json!([{"float": [1]}, {"float16": [1]}]);
        assert!(check(&events).is_err());
    }

    #[test]
    fn mean_pooling_masks_padding_and_normalizes_each_row() {
        let mut data = vec![0.0; 2 * 3 * DIMENSION];
        data[0] = 3.0;
        data[DIMENSION + 1] = 4.0;
        data[2 * DIMENSION + 2] = 999.0; // masked padding
        data[3 * DIMENSION + 2] = 2.0;
        data[4 * DIMENSION + 2] = 2.0;
        data[5 * DIMENSION + 2] = 2.0;
        let output = mean_normalize(&data, &[1, 1, 0, 1, 1, 1], 2, 3).unwrap();
        assert!((output[0][0] - 0.6).abs() < 1e-6);
        assert!((output[0][1] - 0.8).abs() < 1e-6);
        assert_eq!(output[0][2], 0.0);
        assert_eq!(output[1][2], 1.0);
        data[0] = f32::NAN;
        assert!(mean_normalize(&data, &[1, 1, 0, 1, 1, 1], 2, 3).is_err());
    }
}

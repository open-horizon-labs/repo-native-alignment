//! Linux ORT/OpenVINO provider. Discovery uses the C API from the provider's own bundle.
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_char, c_void},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use super::generation::{sha256_bytes, sha256_file};

static INITIALIZED: OnceLock<std::result::Result<(PathBuf, String), String>> = OnceLock::new();
const MAX_LENGTH: usize = 256;
pub(super) const PRECISION: &str = "FP32";

pub(super) struct Loaded {
    pub model: TextEmbedding,
    pub device: String,
    pub version: String,
    pub runtime_digest: String,
    pub assets: BTreeMap<String, String>,
}

/// Require the explicit ordinal to resolve to the intended Intel Arc device.
fn select_arc(devices: &[(String, String)]) -> Result<String> {
    let name = devices
        .iter()
        .find(|(id, _)| id == "GPU.0")
        .map(|(_, name)| name)
        .context("OpenVINO GPU.0 is unavailable")?;
    if !name.contains("Intel") || !name.contains("Arc") {
        bail!("OpenVINO GPU.0 FULL_DEVICE_NAME is not Intel Arc: {name:?}");
    }
    Ok(name.clone())
}

fn bundle_digest(path: &Path) -> Result<String> {
    let mut files = BTreeMap::new();
    let directory = path
        .parent()
        .context("ORT runtime has no parent directory")?;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(".so") && entry.path().is_file() {
            files.insert(name, sha256_file(&entry.path())?);
        }
    }
    files.insert("selected-ort".into(), sha256_file(path)?);
    Ok(sha256_bytes(&serde_json::to_vec(&files)?))
}

fn initialize() -> Result<(PathBuf, String)> {
    if !cfg!(target_os = "linux") {
        bail!("RNA OpenVINO currently supports Linux runtime bundles only");
    }
    let path = PathBuf::from(
        std::env::var_os("ORT_DYLIB_PATH").context("OpenVINO requires ORT_DYLIB_PATH")?,
    )
    .canonicalize()
    .context("failed to load ONNX Runtime from ORT_DYLIB_PATH")?;
    let digest = bundle_digest(&path)?;
    // Cache errors too: ORT cannot safely retry partially initialized global state.
    let initialized = INITIALIZED
        .get_or_init(|| {
            let configured = ort::init_from(&path)
                .map_err(|error| {
                    format!("failed to load ONNX Runtime from ORT_DYLIB_PATH: {error}")
                })?
                .commit();
            if !configured {
                return Err(
                    "ONNX Runtime was initialized before ORT_DYLIB_PATH was applied; restart RNA"
                        .into(),
                );
            }
            Ok((path.clone(), digest.clone()))
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if initialized != &(path.clone(), digest.clone()) {
        bail!("OpenVINO runtime path or bundle bytes changed after initialization; restart RNA");
    }
    Ok((path, digest))
}

// ABI declarations from OpenVINO's openvino/c/ov_core.h. All returned allocations
// are copied and released with the matching C API while the library stays loaded.
#[repr(C)]
struct Devices {
    names: *mut *mut c_char,
    size: usize,
}
#[repr(C)]
struct Version {
    build: *const c_char,
    description: *const c_char,
}

fn discover(bundle: &Path) -> Result<(libloading::Library, String, String)> {
    let path = bundle.join("libopenvino_c.so");
    // SAFETY: an explicitly configured native runtime is executable trusted input,
    // just like ORT_DYLIB_PATH. Function signatures match the stable OpenVINO C API.
    unsafe {
        let library =
            libloading::Library::new(&path).context("load provider-local libopenvino_c.so")?;
        let create =
            library.get::<unsafe extern "C" fn(*mut *mut c_void) -> i32>(b"ov_core_create\0")?;
        let destroy = library.get::<unsafe extern "C" fn(*mut c_void)>(b"ov_core_free\0")?;
        let available = library.get::<unsafe extern "C" fn(*const c_void, *mut Devices) -> i32>(
            b"ov_core_get_available_devices\0",
        )?;
        let free_devices =
            library.get::<unsafe extern "C" fn(*mut Devices)>(b"ov_available_devices_free\0")?;
        let property = library.get::<unsafe extern "C" fn(
            *const c_void,
            *const c_char,
            *const c_char,
            *mut *mut c_char,
        ) -> i32>(b"ov_core_get_property\0")?;
        let free = library.get::<unsafe extern "C" fn(*const c_char)>(b"ov_free\0")?;
        let version = library
            .get::<unsafe extern "C" fn(*mut Version) -> i32>(b"ov_get_openvino_version\0")?;
        let free_version =
            library.get::<unsafe extern "C" fn(*mut Version)>(b"ov_version_free\0")?;
        let mut core = std::ptr::null_mut();
        if create(&mut core) != 0 || core.is_null() {
            bail!("OpenVINO core creation failed");
        }
        let mut devices = Devices {
            names: std::ptr::null_mut(),
            size: 0,
        };
        let result = (|| -> Result<(String, String)> {
            if available(core, &mut devices) != 0 {
                bail!("OpenVINO device enumeration failed");
            }
            if devices.size > 64 || (devices.size > 0 && devices.names.is_null()) {
                bail!("invalid OpenVINO device enumeration");
            }
            let mut names = Vec::new();
            for i in 0..devices.size {
                let id = *devices.names.add(i);
                if id.is_null() {
                    bail!("null OpenVINO device identifier");
                }
                let identifier = CStr::from_ptr(id).to_string_lossy().into_owned();
                if !identifier.starts_with("GPU.") {
                    continue;
                }
                let mut value = std::ptr::null_mut();
                let status = property(core, id, c"FULL_DEVICE_NAME".as_ptr(), &mut value);
                let name = if value.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(value).to_string_lossy().into_owned()
                };
                if !value.is_null() {
                    free(value);
                }
                if status != 0 {
                    bail!("OpenVINO FULL_DEVICE_NAME query failed for {identifier}");
                }
                names.push((identifier, name));
            }
            let device = select_arc(&names)?;
            let mut info = Version {
                build: std::ptr::null(),
                description: std::ptr::null(),
            };
            let status = version(&mut info);
            let build = if info.build.is_null() {
                String::new()
            } else {
                CStr::from_ptr(info.build).to_string_lossy().into_owned()
            };
            free_version(&mut info);
            if status != 0 || build.is_empty() {
                bail!("cannot attest provider-local OpenVINO version");
            }
            Ok((device, build))
        })();
        free_devices(&mut devices);
        destroy(core);
        let (device, version) = result?;
        Ok((library, device, version))
    }
}

/// Reject library-search-path contamination: discovery and ORT must use one bundle.
fn verify_loaded_bundle(bundle: &Path) -> Result<()> {
    let maps = std::fs::read_to_string("/proc/self/maps")
        .context("verify loaded OpenVINO runtime libraries")?;
    let mut seen_provider = false;
    let mut seen_gpu = false;
    for line in maps.lines() {
        let Some(path) = line.split_whitespace().nth(5) else {
            continue;
        };
        let path = Path::new(path);
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with("libopenvino") || name.starts_with("libonnxruntime")) {
            continue;
        }
        if path.canonicalize()?.parent() != Some(bundle) {
            bail!(
                "OpenVINO runtime library loaded outside configured ORT bundle: {}",
                path.display()
            );
        }
        seen_provider |= name == "libonnxruntime_providers_openvino.so";
        seen_gpu |= name == "libopenvino_intel_gpu_plugin.so";
    }
    if !seen_provider || !seen_gpu {
        bail!("OpenVINO provider and Intel GPU plugin were not loaded");
    }
    Ok(())
}

pub(super) fn load(probe: &str) -> Result<Loaded> {
    let (path, runtime_digest) = initialize()?;
    let bundle = path.parent().context("ORT bundle directory missing")?;
    let (_library, device, version) = discover(bundle)?;
    let directory = PathBuf::from(std::env::var_os("RNA_OPENVINO_MODEL_DIR")
        .context("set RNA_OPENVINO_MODEL_DIR to the MiniLM ONNX snapshot containing model.onnx and tokenizer JSON files")?);
    if !directory.is_absolute() {
        bail!("RNA_OPENVINO_MODEL_DIR must be an absolute path");
    }
    let mut assets = BTreeMap::new();
    let mut read = |name: &str| -> Result<Vec<u8>> {
        let bytes = std::fs::read(directory.join(name))
            .with_context(|| format!("read OpenVINO model asset {name}"))?;
        assets.insert(name.to_string(), sha256_bytes(&bytes));
        Ok(bytes)
    };
    let onnx = read("model.onnx")?;
    let tokenizer = TokenizerFiles {
        tokenizer_file: read("tokenizer.json")?,
        config_file: read("config.json")?,
        special_tokens_map_file: read("special_tokens_map.json")?,
        tokenizer_config_file: read("tokenizer_config.json")?,
    };
    // Pass precisely the hashed bytes; no second cache lookup or download can change identity.
    let model = UserDefinedEmbeddingModel::new(onnx, tokenizer).with_pooling(Pooling::Mean);
    let provider = ort::ep::OpenVINO::default()
        .with_device_type("GPU.0")
        .with_precision(PRECISION)
        .build()
        .error_on_failure();
    let options = InitOptionsUserDefined::new()
        .with_max_length(MAX_LENGTH)
        .with_execution_providers(vec![provider]);
    let mut model =
        TextEmbedding::try_new_from_user_defined(model, options).context("load OpenVINO MiniLM")?;
    let vectors = model
        .embed([probe], None)
        .context("OpenVINO canonical embedding readiness probe")?;
    validate_probe(&vectors)?;
    verify_loaded_bundle(bundle)?;
    Ok(Loaded {
        model,
        device,
        version,
        runtime_digest,
        assets,
    })
}

fn validate_probe(vectors: &[Vec<f32>]) -> Result<()> {
    if vectors.len() != 1 || vectors[0].len() != 384 || vectors[0].iter().any(|v| !v.is_finite()) {
        bail!("OpenVINO readiness probe must produce one finite [1,384] embedding");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openvino_device_mismatch_and_missing_intel_are_rejected() {
        for devices in [
            vec![],
            vec![("GPU.0".into(), "NVIDIA".into())],
            vec![("GPU.1".into(), "Intel Arc".into())],
            vec![("GPU".into(), "Intel Arc".into())],
            vec![("GPU.0".into(), String::new())],
        ] {
            assert!(select_arc(&devices).is_err());
        }
        assert_eq!(
            select_arc(&[("GPU.0".into(), "Intel(R) Arc(TM) Graphics (iGPU)".into())]).unwrap(),
            "Intel(R) Arc(TM) Graphics (iGPU)"
        );
    }
    #[test]
    fn openvino_probe_rejects_nonfinite_and_incorrect_shapes() {
        assert!(validate_probe(&[vec![0.0; 384]]).is_ok());
        for vectors in [
            vec![],
            vec![vec![]],
            vec![vec![0.0; 383]],
            vec![vec![0.0; 384]; 2],
            vec![vec![f32::NAN; 384]],
            vec![vec![f32::INFINITY; 384]],
        ] {
            assert!(validate_probe(&vectors).is_err());
        }
    }
    #[test]
    fn openvino_runtime_bytes_invalidate_identity_at_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let ort = dir.path().join("libonnxruntime.so");
        let provider = dir.path().join("libopenvino.so");
        std::fs::write(&ort, b"runtime").unwrap();
        std::fs::write(&provider, b"provider v1").unwrap();
        let before = bundle_digest(&ort).unwrap();
        std::fs::write(&provider, b"provider v2").unwrap();
        assert_ne!(bundle_digest(&ort).unwrap(), before);
    }
}
